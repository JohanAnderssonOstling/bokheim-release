use std::cell::RefCell;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{App, Bounds, Pixels, point, px, size};
use reader_ui::ReaderPreferences;
use serde::{Deserialize, Serialize};

use crate::blocking_worker::BlockingWorker;

const SESSION_VERSION: u32 = 1;

fn default_kobo_auto_rotate() -> bool {
    true
}

/// Geometry for the one application window.
///
/// This used to describe a separate reader window. Keeping the same serialized
/// shape lets existing desktop sessions migrate without losing their layout.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowGeometry {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl WindowGeometry {
    pub fn from_bounds(bounds: Bounds<Pixels>) -> Self {
        Self { x: f32::from(bounds.origin.x), y: f32::from(bounds.origin.y), width: f32::from(bounds.size.width), height: f32::from(bounds.size.height) }
    }

    pub fn bounds(self) -> Bounds<Pixels> {
        Bounds { origin: point(px(self.x), px(self.y)), size: size(px(self.width), px(self.height)) }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UiSession {
    version: u32,
    #[serde(default = "default_kobo_auto_rotate")]
    pub kobo_auto_rotate: bool,
    pub reader_preferences: ReaderPreferences,
    #[serde(default, alias = "reader_window")]
    pub window: Option<WindowGeometry>,
}

impl Default for UiSession {
    fn default() -> Self {
        Self { version: SESSION_VERSION, kobo_auto_rotate: default_kobo_auto_rotate(), reader_preferences: ReaderPreferences::default(), window: None }
    }
}

impl UiSession {
    pub fn load(path: &Path) -> Self {
        let Ok(bytes) = fs::read(path) else {
            return Self::default();
        };
        match serde_json::from_slice::<Self>(&bytes) {
            Ok(session) if session.version == SESSION_VERSION => session,
            Ok(_) => {
                log::warn!("ignoring GPUI session with an unsupported version");
                Self::default()
            }
            Err(error) => {
                log::warn!("failed to read GPUI session: {error}");
                Self::default()
            }
        }
    }

    pub fn save(&self, path: &Path) {
        if let Some(parent) = path.parent()
            && let Err(error) = fs::create_dir_all(parent)
        {
            log::error!("failed to create GPUI session directory: {error}");
            return;
        }
        match serde_json::to_vec_pretty(self) {
            Ok(bytes) => {
                if let Err(error) = atomic_session_write(path, |file| file.write_all(&bytes)) {
                    log::error!("failed to save GPUI session: {error}");
                }
            }
            Err(error) => {
                log::error!("failed to serialize GPUI session: {error}");
            }
        }
    }
}

// Keep the previous session intact until all new bytes are durably written.
// The temporary file must be adjacent so replacement stays on one filesystem.
fn atomic_session_write(path: &Path, write: impl FnOnce(&mut fs::File) -> io::Result<()>) -> io::Result<()> {
    let parent = path.parent().filter(|parent| !parent.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let mut staged = tempfile::Builder::new().prefix(".gpui-session-").tempfile_in(parent)?;
    write(staged.as_file_mut())?;
    staged.as_file().sync_all()?;
    staged.persist(path).map_err(|error| error.error)?;
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

pub fn session_path(app_data: &Path) -> PathBuf {
    app_data.join("gpui-ui-session.json")
}

#[derive(Clone)]
pub struct SessionSaveScheduler {
    session: Rc<RefCell<UiSession>>,
    path: PathBuf,
    state: Rc<RefCell<SaveSchedule>>,
    blocking_worker: BlockingWorker,
}

#[derive(Default)]
struct SaveSchedule {
    deadline: Option<Instant>,
    worker_running: bool,
}

impl SessionSaveScheduler {
    pub fn new(session: Rc<RefCell<UiSession>>, path: PathBuf, blocking_worker: BlockingWorker) -> Self {
        Self { session, path, state: Rc::new(RefCell::new(SaveSchedule::default())), blocking_worker }
    }

    pub fn schedule(&self, cx: &mut App) {
        {
            let mut state = self.state.borrow_mut();
            state.deadline = Some(Instant::now() + Duration::from_millis(300));
            if state.worker_running {
                return;
            }
            state.worker_running = true;
        }

        let scheduler = self.clone();
        let executor = cx.background_executor().clone();
        cx.spawn(async move |_| {
            loop {
                let wait = {
                    let state = scheduler.state.borrow();
                    let Some(deadline) = state.deadline else {
                        break;
                    };
                    deadline.saturating_duration_since(Instant::now())
                };
                executor.timer(wait).await;

                let should_save = {
                    let mut state = scheduler.state.borrow_mut();
                    match state.deadline {
                        Some(deadline) if deadline <= Instant::now() => {
                            state.deadline = None;
                            true
                        }
                        Some(_) => false,
                        None => {
                            state.worker_running = false;
                            break;
                        }
                    }
                };
                if should_save {
                    let session = scheduler.session.borrow().clone();
                    let path = scheduler.path.clone();
                    let save = scheduler.blocking_worker.dispatch(move || session.save(&path));
                    let _ = save.recv().await;
                    let mut state = scheduler.state.borrow_mut();
                    if state.deadline.is_none() {
                        state.worker_running = false;
                        break;
                    }
                }
            }
        })
        .detach();
    }

    pub fn flush(&self) {
        {
            let mut state = self.state.borrow_mut();
            state.deadline = None;
            state.worker_running = false;
        }
        let session = self.session.borrow().clone();
        let path = self.path.clone();
        let save = self.blocking_worker.dispatch(move || session.save(&path));
        let _ = save.recv_blocking();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_session_write_preserves_previous_bytes_and_cleans_staging() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("session.json");
        UiSession::default().save(&path);
        let previous = fs::read(&path).unwrap();
        let result = atomic_session_write(&path, |file| {
            file.write_all(b"partial JSON")?;
            Err(io::Error::other("simulated disk-full write failure"))
        });
        assert!(result.is_err());
        assert_eq!(fs::read(&path).unwrap(), previous);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn session_save_atomically_replaces_an_existing_session() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("session.json");
        UiSession::default().save(&path);
        let geometry = WindowGeometry { x: 1.0, y: 2.0, width: 900.0, height: 700.0 };
        UiSession { window: Some(geometry), ..UiSession::default() }.save(&path);
        assert_eq!(UiSession::load(&path).window, Some(geometry));
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn legacy_reader_window_geometry_migrates_to_application_window() {
        let geometry = WindowGeometry { x: 12.0, y: 24.0, width: 900.0, height: 700.0 };
        let mut session_value = serde_json::to_value(UiSession { window: Some(geometry), ..UiSession::default() }).expect("serialize session");
        let object = session_value.as_object_mut().expect("session object");
        let geometry_value = object.remove("window").expect("serialized window geometry");
        object.insert("reader_window".to_owned(), geometry_value);

        let session: UiSession = serde_json::from_value(session_value).expect("deserialize legacy session");

        assert_eq!(session.window, Some(geometry));
    }
}
