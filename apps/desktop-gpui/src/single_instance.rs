use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use fs2::FileExt;

#[doc(hidden)]
pub enum DesktopInstance {
    Primary(PrimaryInstance),
    Secondary,
}

#[doc(hidden)]
pub struct PrimaryInstance {
    _lock: File,
    stop: Arc<AtomicBool>,
    pub activations: async_channel::Receiver<()>,
}

impl DesktopInstance {
    pub fn acquire(app_data_dir: &Path) -> io::Result<Self> {
        Self::acquire_with_lock(app_data_dir, FileExt::try_lock_exclusive)
    }

    fn acquire_with_lock(app_data_dir: &Path, try_lock: impl FnOnce(&File) -> io::Result<()>) -> io::Result<Self> {
        let lock_path = app_data_dir.join("desktop-instance.lock");
        let activation_path = app_data_dir.join("desktop-instance.activate");
        let lock = OpenOptions::new().create(true).read(true).write(true).open(lock_path)?;
        match try_lock(&lock) {
            Ok(()) => {}
            Err(error) if error.raw_os_error().is_some_and(|code| Some(code) == fs2::lock_contended_error().raw_os_error()) => {
                OpenOptions::new().create(true).write(true).truncate(true).open(activation_path)?;
                return Ok(Self::Secondary);
            }
            Err(error) => return Err(error),
        }

        let _ = std::fs::remove_file(&activation_path);
        let (activation_tx, activations) = async_channel::unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        spawn_activation_listener(activation_path, stop.clone(), activation_tx)?;
        Ok(Self::Primary(PrimaryInstance { _lock: lock, stop, activations }))
    }
}

fn spawn_activation_listener(path: PathBuf, stop: Arc<AtomicBool>, activations: async_channel::Sender<()>) -> io::Result<()> {
    std::thread::Builder::new().name("bokheim-instance-activation".to_owned()).spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            if path.exists() && std::fs::remove_file(&path).is_ok() && activations.send_blocking(()).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    })?;
    Ok(())
}

impl Drop for PrimaryInstance {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = FileExt::unlock(&self._lock);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_failures_do_not_signal_a_nonexistent_primary() {
        for kind in [io::ErrorKind::Unsupported, io::ErrorKind::PermissionDenied, io::ErrorKind::Other] {
            let directory = tempfile::tempdir().unwrap();
            let result = DesktopInstance::acquire_with_lock(directory.path(), |_| Err(io::Error::new(kind, "lock failure")));
            assert!(matches!(result, Err(error) if error.kind() == kind));
            assert!(!directory.path().join("desktop-instance.activate").exists());
        }
    }

    #[test]
    fn second_process_signals_primary_instead_of_becoming_an_owner() {
        let directory = tempfile::tempdir().unwrap();
        let DesktopInstance::Primary(primary) = DesktopInstance::acquire(directory.path()).unwrap() else {
            panic!("first instance was not primary");
        };
        assert!(matches!(DesktopInstance::acquire(directory.path()).unwrap(), DesktopInstance::Secondary));

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            if primary.activations.try_recv().is_ok() {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "primary did not receive activation");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
