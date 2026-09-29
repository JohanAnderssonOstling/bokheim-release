//! Optional application-owned preparation before removing a configured library.
use gpui::{App, Global};
use std::{cell::RefCell, collections::HashSet, future::Future, pin::Pin, rc::Rc};
use sync_common::LibraryId;

pub type LibraryRemovalPreparation = Pin<Box<dyn Future<Output = Result<(), String>>>>;
pub struct LibraryRemovalHook(pub Rc<dyn Fn(LibraryId, &mut App) -> LibraryRemovalPreparation>);
impl Global for LibraryRemovalHook {}

#[derive(Default)]
struct RemovingLibraries(Rc<RefCell<HashSet<LibraryId>>>);
impl Global for RemovingLibraries {}

/// Held through preparation AND backend deletion, including cancellation.
pub(crate) struct LibraryRemovalGuard {
    library: LibraryId,
    removing: Rc<RefCell<HashSet<LibraryId>>>,
}

impl Drop for LibraryRemovalGuard {
    fn drop(&mut self) {
        self.removing.borrow_mut().remove(&self.library);
    }
}

pub fn library_is_being_removed(library: LibraryId, cx: &App) -> bool {
    cx.try_global::<RemovingLibraries>().is_some_and(|state| state.0.borrow().contains(&library))
}

pub(crate) fn begin_library_removal(library: LibraryId, cx: &mut App) -> Result<LibraryRemovalGuard, String> {
    if cx.try_global::<RemovingLibraries>().is_none() {
        cx.set_global(RemovingLibraries::default());
    }
    let removing = cx.global::<RemovingLibraries>().0.clone();
    if !removing.borrow_mut().insert(library) {
        return Err("Library removal is already in progress".into());
    }
    Ok(LibraryRemovalGuard { library, removing })
}

pub(crate) fn prepare_library_removal(library: LibraryId, cx: &mut App) -> LibraryRemovalPreparation {
    let hook = cx.try_global::<LibraryRemovalHook>().map(|hook| hook.0.clone());
    match hook {
        Some(hook) => hook(library, cx),
        None => Box::pin(async { Ok(()) }),
    }
}

pub(crate) async fn prepared_removal<T>(preparation: LibraryRemovalPreparation, remove: impl Future<Output = Result<T, String>>) -> Result<T, String> {
    preparation.await?;
    remove.await
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::FutureExt;
    use std::cell::Cell;

    #[gpui::test]
    fn removal_guard_excludes_duplicates_and_releases_on_drop(cx: &mut gpui::TestAppContext) {
        let library = LibraryId::from_u128(3);
        let other = LibraryId::from_u128(4);
        let guard = cx.update(|cx| begin_library_removal(library, cx).unwrap());
        cx.update(|cx| {
            assert!(library_is_being_removed(library, cx));
            assert!(!library_is_being_removed(other, cx));
            assert!(begin_library_removal(library, cx).is_err());
        });
        drop(guard);
        cx.update(|cx| {
            assert!(!library_is_being_removed(library, cx));
            assert!(begin_library_removal(library, cx).is_ok());
        });
    }

    #[gpui::test]
    fn removal_guard_survives_preparation_until_deletion_is_cancelled(cx: &mut gpui::TestAppContext) {
        let library = LibraryId::from_u128(3);
        let guard = cx.update(|cx| begin_library_removal(library, cx).unwrap());
        let mut operation = Box::pin(async move {
            let _guard = guard;
            prepared_removal(Box::pin(async { Ok(()) }), std::future::pending::<Result<(), String>>()).await
        });
        let mut poll = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(operation.as_mut().poll(&mut poll).is_pending());
        cx.update(|cx| assert!(library_is_being_removed(library, cx)));
        drop(operation);
        cx.update(|cx| assert!(!library_is_being_removed(library, cx)));
    }

    #[gpui::test]
    fn removal_guard_releases_after_success_or_either_failure(cx: &mut gpui::TestAppContext) {
        let library = LibraryId::from_u128(3);
        for (prepare_result, delete_result, expected) in [(Ok(()), Ok(()), Ok(())), (Err("save failed".to_owned()), Ok(()), Err("save failed".to_owned())), (Ok(()), Err("delete failed".to_owned()), Err("delete failed".to_owned()))] {
            let guard = cx.update(|cx| begin_library_removal(library, cx).unwrap());
            let result = async {
                let _guard = guard;
                prepared_removal(Box::pin(async move { prepare_result }), async {
                    cx.update(|cx| assert!(library_is_being_removed(library, cx)));
                    delete_result
                })
                .await
            }
            .now_or_never()
            .unwrap();
            assert_eq!(result, expected);
            cx.update(|cx| assert!(!library_is_being_removed(library, cx)));
        }
        let guard = cx.update(|cx| begin_library_removal(library, cx).unwrap());
        let unpolled = async move {
            let _guard = guard;
            std::future::pending::<()>().await;
        };
        drop(unpolled);
        cx.update(|cx| assert!(!library_is_being_removed(library, cx)));
    }

    #[gpui::test]
    fn installed_hook_receives_the_library_before_removal(cx: &mut gpui::TestAppContext) {
        let library = LibraryId::from_u128(3);
        let called = Rc::new(Cell::new(false));
        let observed = called.clone();
        cx.update(|cx| {
            cx.set_global(LibraryRemovalHook(Rc::new(move |id, _| {
                assert_eq!(id, library);
                observed.set(true);
                Box::pin(async { Ok(()) })
            })))
        });
        let preparation = cx.update(|cx| prepare_library_removal(library, cx));
        assert!(called.get());
        assert_eq!(preparation.now_or_never(), Some(Ok(())));
    }

    #[test]
    fn failed_preparation_does_not_run_deletion() {
        let removed = Cell::new(false);
        let result = prepared_removal(Box::pin(async { Err("progress save failed".into()) }), async {
            removed.set(true);
            Ok(())
        })
        .now_or_never()
        .unwrap();
        assert_eq!(result, Err("progress save failed".into()));
        assert!(!removed.get());
    }

    #[test]
    fn deletion_waits_for_preparation() {
        let saved = Cell::new(false);
        let result = prepared_removal(Box::pin(async { Ok(()) }), async {
            saved.set(true);
            Ok(())
        })
        .now_or_never();
        assert_eq!(result, Some(Ok(())));
        assert!(saved.get());
        let removed = Cell::new(false);
        let result = prepared_removal(Box::pin(std::future::pending()), async {
            removed.set(true);
            Ok(())
        })
        .now_or_never();
        assert!(result.is_none());
        assert!(!removed.get());
    }
}
