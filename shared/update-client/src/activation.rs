//! Native startup transaction. The host holds application ownership and UpdateLock.
//! Durable backups precede mutations; recovery is repeatable after any interruption.
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Replacement {
    pub destination: PathBuf,
    pub previous: Option<PathBuf>,
    pub prepared: PathBuf,
    pub sqlite: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum State {
    Applying,
    Trial,
    Committed,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Journal {
    pub plan_id: String,
    pub previous_version: String,
    pub next_version: String,
    pub appimage: Option<PathBuf>,
    pub replacements: Vec<Replacement>,
    pub state: State,
}

pub fn sync_dir(path: &Path) -> Result<(), String> {
    crate::native_files::sync_dir(path)
}
pub fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    atomic_bytes(path, &bytes)
}
fn atomic_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let part = path.with_extension("writing");
    let mut f = File::create(&part).map_err(|e| e.to_string())?;
    f.write_all(bytes).and_then(|_| f.sync_all()).map_err(|e| e.to_string())?;
    drop(f);
    crate::native_files::replace(&part, path)
}
pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    serde_json::from_reader(File::open(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

/// The SQLite backup API includes committed WAL pages and never copies a live
/// database by reading its main file alone. Also used to restore the prior schema.
pub fn copy_database(source: &Path, destination: &Path) -> Result<(), String> {
    let src = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| e.to_string())?;
    let mut dst = Connection::open(destination).map_err(|e| e.to_string())?;
    dst.busy_timeout(std::time::Duration::from_secs(5)).map_err(|e| e.to_string())?;
    {
        let backup = rusqlite::backup::Backup::new(&src, &mut dst).map_err(|e| e.to_string())?;
        match backup.step(-1).map_err(|e| e.to_string())? {
            rusqlite::backup::StepResult::Done => {}
            _ => return Err("database is busy; backup must be retried".into()),
        }
    }
    let (busy, _, _): (i64, i64, i64) = dst.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map_err(|e| e.to_string())?;
    if busy != 0 {
        return Err("database checkpoint is busy".into());
    }
    drop(dst);
    crate::native_files::sync_file(destination)?;
    sync_dir(destination.parent().ok_or("database has no parent")?)
}

pub fn copy_file(source: &Path, destination: &Path) -> Result<(), String> {
    let part = destination.with_extension("update-writing");
    fs::copy(source, &part).map_err(|e| e.to_string())?;
    crate::native_files::sync_file(&part)?;
    crate::native_files::replace(&part, destination)
}

fn identical(left: &Path, right: &Path) -> Result<bool, String> {
    if !right.try_exists().map_err(|e| e.to_string())? {
        return Ok(false);
    }
    let mut a = File::open(left).map_err(|e| e.to_string())?;
    let mut b = File::open(right).map_err(|e| e.to_string())?;
    if a.metadata().map_err(|e| e.to_string())?.len() != b.metadata().map_err(|e| e.to_string())?.len() {
        return Ok(false);
    }
    let mut x = [0u8; 65536];
    let mut y = [0u8; 65536];
    loop {
        let n = a.read(&mut x).map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(true);
        }
        b.read_exact(&mut y[..n]).map_err(|e| e.to_string())?;
        if x[..n] != y[..n] {
            return Ok(false);
        }
    }
}

impl Journal {
    pub fn save(&self, root: &Path) -> Result<(), String> {
        write_json(&root.join("activation.json"), self)
    }
    pub fn load(root: &Path) -> Result<Option<Self>, String> {
        let path = root.join("activation.json");
        if !path.try_exists().map_err(|e| e.to_string())? {
            return Ok(None);
        }
        read_json(&path).map(Some)
    }
    pub fn promote(&mut self, root: &Path) -> Result<(), String> {
        // This durable record makes even an interrupted first replacement recoverable.
        self.state = State::Applying;
        self.save(root)?;
        for entry in &self.replacements {
            if entry.sqlite {
                copy_database(&entry.prepared, &entry.destination)?;
            } else {
                copy_file(&entry.prepared, &entry.destination)?;
            }
        }
        self.state = State::Trial;
        self.save(root)
    }
    pub fn commit(&mut self, root: &Path) -> Result<(), String> {
        if self.state != State::Trial {
            return Err("update trial is not ready to commit".into());
        }
        self.state = State::Committed;
        self.save(root)
    }
    pub fn recover(&self, root: &Path) -> Result<(), String> {
        if self.state == State::Committed {
            return Ok(());
        }
        for entry in self.replacements.iter().rev() {
            if let Some(previous) = &entry.previous {
                if entry.sqlite {
                    copy_database(previous, &entry.destination)?;
                } else if !identical(previous, &entry.destination)? {
                    copy_file(previous, &entry.destination)?;
                }
            } else if entry.destination.try_exists().map_err(|e| e.to_string())? {
                crate::native_files::remove(&entry.destination)?;
            }
        }
        // Host clears only after its coordinator has recorded recovery too.
        sync_dir(root)
    }
    pub fn clear(root: &Path) -> Result<(), String> {
        crate::native_files::remove(&root.join("activation.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interrupted_promotion_restores_wal_data_and_files_repeatably() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        let db = Connection::open(p.join("live.db")).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE book(v); INSERT INTO book VALUES('original'); PRAGMA user_version=1;").unwrap();
        copy_database(&p.join("live.db"), &p.join("prior.db")).unwrap();
        drop(db);
        copy_database(&p.join("prior.db"), &p.join("next.db")).unwrap();
        Connection::open(p.join("next.db")).unwrap().execute_batch("UPDATE book SET v='new'; PRAGMA user_version=2;").unwrap();
        fs::write(p.join("app"), b"old binary").unwrap();
        fs::write(p.join("prior-app"), b"old binary").unwrap();
        let mut journal = Journal {
            plan_id: "test".into(),
            previous_version: "1".into(),
            next_version: "2".into(),
            appimage: None,
            state: State::Applying,
            replacements: vec![
                Replacement { destination: p.join("live.db"), previous: Some(p.join("prior.db")), prepared: p.join("next.db"), sqlite: true },
                Replacement { destination: p.join("app"), previous: Some(p.join("prior-app")), prepared: p.join("missing"), sqlite: false },
            ],
        };
        assert!(journal.promote(p).is_err());
        let interrupted = Journal::load(p).unwrap().unwrap();
        interrupted.recover(p).unwrap();
        interrupted.recover(p).unwrap();
        let db = Connection::open(p.join("live.db")).unwrap();
        assert_eq!(db.query_row("SELECT v FROM book", [], |r| r.get::<_, String>(0)).unwrap(), "original");
        assert_eq!(db.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0)).unwrap(), 1);
        assert_eq!(fs::read(p.join("app")).unwrap(), b"old binary");
    }
    #[test]
    fn committed_trial_is_never_rolled_back() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        fs::write(p.join("new"), b"new").unwrap();
        let mut journal = Journal {
            plan_id: "test".into(),
            previous_version: "1".into(),
            next_version: "1".into(),
            appimage: None,
            state: State::Applying,
            replacements: vec![Replacement { destination: p.join("active"), previous: None, prepared: p.join("new"), sqlite: false }],
        };
        journal.promote(p).unwrap();
        journal.commit(p).unwrap();
        journal.recover(p).unwrap();
        assert_eq!(fs::read(p.join("active")).unwrap(), b"new");
        Journal::clear(p).unwrap();
        assert!(Journal::load(p).unwrap().is_none());
    }
}
