//! Import failure text, independent of notification widgets.

use library_backend::ImportFailure;
pub(crate) fn import_failure_message(failure: &ImportFailure) -> String {
    let summary = import_failure_summary(failure);
    if failure.failure.is_some() {
        format!("{summary}. {}", ui_components::book_transfer_error_message(failure.failure))
    } else { summary }
}

fn import_failure_summary(failure: &ImportFailure) -> String {
    use library_backend::ImportFailureKind;
    match failure.kind {
        ImportFailureKind::File => match failure.path.last() {
            Some(name) => format!("Could not import “{name}”"),
            None => "Could not import a book".into(),
        },
        ImportFailureKind::Folder => {
            if let Some(hidden) = failure.path.iter().position(|part| part.starts_with('.')) {
                return match hidden.checked_sub(1).and_then(|i| failure.path.get(i)) {
                    Some(parent) => format!("A folder in “{parent}” could not be imported"),
                    None => "A folder could not be imported".into(),
                };
            }
            match failure.path.last() {
                Some(name) => format!("Could not import folder “{name}”"),
                None => "A folder could not be imported".into(),
            }
        }
    }
}
