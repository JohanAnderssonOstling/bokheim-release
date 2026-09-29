//! Native pool lock diagnostics. Never log SQL text or bound values.
//! Trace callbacks run on the connection's owning thread; the registry contains
//! only snapshots, never handles that another thread may dereference.
use rusqlite::{ffi, Connection};
use std::collections::HashMap;
use std::ffi::{c_int, c_uint, c_void, CStr};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const SLOW: Duration = Duration::from_secs(1);

struct State {
    database: String,
    caller: &'static str,
    writer: Option<(Instant, &'static str)>,
    statement: Option<(Instant, StatementLabel)>,
    waiting: Option<Instant>,
    timeout: Duration,
}

fn registry() -> &'static Mutex<HashMap<usize, State>> {
    static STATES: OnceLock<Mutex<HashMap<usize, State>>> = OnceLock::new();
    STATES.get_or_init(Mutex::default)
}

pub(crate) fn install(conn: &Connection) {
    // Preserve the configured budget (normally 30 seconds for libraries).
    let Ok(timeout_ms) = conn.pragma_query_value(None, "busy_timeout", |row| row.get::<_, u32>(0)) else { return };
    // SAFETY: callbacks are removed by SQLite when this connection closes. The
    // context is an opaque integer key, not a borrowed Rust allocation.
    unsafe {
        let db = conn.handle();
        let filename = ffi::sqlite3_db_filename(db, c"main".as_ptr());
        let mut database = if filename.is_null() { String::new() } else { CStr::from_ptr(filename).to_string_lossy().into_owned() };
        if database.is_empty() {
            database = format!("memory:{:x}", db as usize);
        }
        registry().lock().unwrap_or_else(|p| p.into_inner()).insert(db as usize, State { database, caller: "SQLite connection", writer: None, statement: None, waiting: None, timeout: Duration::from_millis(u64::from(timeout_ms)) });
        ffi::sqlite3_trace_v2(db, ffi::SQLITE_TRACE_STMT | ffi::SQLITE_TRACE_PROFILE | ffi::SQLITE_TRACE_CLOSE, Some(trace), db.cast());
        // Same timeout budget, now with attribution and bounded retry sleeps.
        ffi::sqlite3_busy_handler(db, Some(busy), db.cast());
    }
}

pub(crate) fn command_failed(conn: &Connection) {
    // Include commands that fail immediately (e.g. a stale read-to-write
    // upgrade), for which SQLite deliberately does not invoke a busy handler.
    let message = {
        let states = registry().lock().unwrap_or_else(|p| p.into_inner());
        let id = unsafe { conn.handle() } as usize;
        states.get(&id).map(|state| format!("sqlite_storage_command_failed database={:?} conn={id:x} caller={} owners=[{}] (see operation error for cause)", state.database, state.caller, owners(&states, id)))
    };
    if let Some(message) = message {
        log::warn!("{message}");
    }
}

fn owners(states: &HashMap<usize, State>, waiter: usize) -> String {
    let Some(waiting) = states.get(&waiter) else { return "unknown".into() };
    let mut result = Vec::new();
    for (id, state) in states {
        if *id == waiter || state.database != waiting.database {
            continue;
        }
        if let Some((since, caller)) = &state.writer {
            result.push(format!("writer conn={id:x} held_ms={} caller={caller}", since.elapsed().as_millis()));
        } else if let Some((since, operation)) = &state.statement {
            // An autocommit write may still be inside sqlite3_step, before its
            // profile callback. Do not falsely label it a confirmed lock owner.
            result.push(format!("active_candidate conn={id:x} elapsed_ms={} caller={} operation={operation}", since.elapsed().as_millis(), state.caller));
        }
    }
    if result.is_empty() {
        "unknown (external connection/process or unobserved lock)".into()
    } else {
        result.join("; ")
    }
}

unsafe extern "C" fn busy(context: *mut c_void, count: c_int) -> c_int {
    // Never unwind through SQLite's C ABI. On a diagnostic panic, stop retrying.
    std::panic::catch_unwind(|| {
        let id = context as usize;
        let (elapsed, timeout, message) = {
            let mut states = registry().lock().unwrap_or_else(|p| p.into_inner());
            let Some(state) = states.get_mut(&id) else { return 0 };
            if count == 0 {
                state.waiting = Some(Instant::now());
            }
            let elapsed = state.waiting.get_or_insert_with(Instant::now).elapsed();
            let timeout = state.timeout;
            let message = if count == 0 || count % 100 == 0 || elapsed >= timeout {
                let caller = state.caller;
                let database = state.database.clone();
                Some(format!("sqlite_lock_wait database={database:?} waiter={id:x} caller={caller} wait_ms={} timed_out={} owners=[{}]", elapsed.as_millis(), elapsed >= timeout, owners(&states, id)))
            } else {
                None
            };
            (elapsed, timeout, message)
        };
        if let Some(message) = message {
            log::warn!("{message}");
        }
        if elapsed >= timeout {
            return 0;
        }
        std::thread::sleep(Duration::from_millis(10).min(timeout - elapsed));
        1
    })
    .unwrap_or(0)
}

// A stable fingerprint correlates statements without exposing literals, book
// names, credentials, or the expanded SQL supplied to SQLITE_TRACE_STMT.
#[derive(Debug, PartialEq)]
struct StatementLabel(&'static str, u64);

impl std::fmt::Display for StatementLabel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}#{:016x}", self.0, self.1)
    }
}

fn fingerprint(sql: &[u8]) -> StatementLabel {
    let hash = sql.iter().fold(0xcbf29ce484222325_u64, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3));
    let first = sql.split(|c| c.is_ascii_whitespace()).find(|word| !word.is_empty()).unwrap_or_default();
    let verb = [
        include_str!("sql/database_diagnostics/fingerprint_select.sql"),
        "INSERT",
        "UPDATE",
        "DELETE",
        "BEGIN",
        include_str!("sql/database_diagnostics/fingerprint_commit.sql"),
        include_str!("sql/database_diagnostics/fingerprint_rollback.sql"),
        "PRAGMA",
        "CREATE",
        "WITH",
    ]
    .into_iter()
    .find(|word| first.eq_ignore_ascii_case(word.as_bytes()))
    .unwrap_or("OTHER");
    StatementLabel(verb, hash)
}

unsafe extern "C" fn trace(event: c_uint, context: *mut c_void, statement: *mut c_void, detail: *mut c_void) -> c_int {
    let _ = std::panic::catch_unwind(|| {
        let id = context as usize;
        let mut messages = Vec::new();
        {
            let mut states = registry().lock().unwrap_or_else(|p| p.into_inner());
            if event == ffi::SQLITE_TRACE_CLOSE {
                states.remove(&id);
                return;
            }
            let Some(state) = states.get_mut(&id) else { return };
            // SAFETY: SQLite supplies live statement/database pointers for these
            // synchronous events. Only inspect this callback's own connection.
            unsafe {
                if event == ffi::SQLITE_TRACE_STMT {
                    // Ignore trigger subprogram notifications for this statement.
                    if !detail.is_null() && CStr::from_ptr(detail.cast()).to_bytes().starts_with(b"--") {
                        return;
                    }
                    let sql = ffi::sqlite3_sql(statement.cast());
                    let operation = if sql.is_null() { StatementLabel("unknown", 0) } else { fingerprint(CStr::from_ptr(sql).to_bytes()) };
                    state.statement = Some((Instant::now(), operation));
                } else if event == ffi::SQLITE_TRACE_PROFILE {
                    if let Some((since, operation)) = state.statement.take() {
                        if since.elapsed() >= SLOW {
                            messages.push(format!("sqlite_slow_statement database={:?} conn={id:x} caller={} elapsed_ms={} operation={operation}", state.database, state.caller, since.elapsed().as_millis()));
                        }
                    }
                    if let Some(since) = state.waiting.take() {
                        messages.push(format!("sqlite_lock_wait_finished database={:?} conn={id:x} caller={} wait_ms={} (statement finished; not necessarily successful)", state.database, state.caller, since.elapsed().as_millis()));
                    }
                    if ffi::sqlite3_txn_state(context.cast(), c"main".as_ptr()) == ffi::SQLITE_TXN_WRITE {
                        state.writer.get_or_insert_with(|| (Instant::now(), state.caller));
                    } else if let Some((since, caller)) = state.writer.take() {
                        if since.elapsed() >= SLOW {
                            messages.push(format!("sqlite_long_write_transaction database={:?} conn={id:x} caller={caller} held_ms={}", state.database, since.elapsed().as_millis()));
                        }
                    }
                }
            }
        }
        for message in messages {
            log::warn!("{message}");
        }
    });
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_real_writer_and_cleans_up_after_rollback_and_close() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("locks.db");
        let writer = Connection::open(&path).unwrap();
        writer.execute_batch(include_str!("sql/database_diagnostics/contending_writer_is_identified_and_waiter_resumes_pragma.sql")).unwrap();
        install(&writer);
        let waiter = Connection::open(&path).unwrap();
        install(&waiter);
        let writer_id = unsafe { writer.handle() } as usize;
        let waiter_id = unsafe { waiter.handle() } as usize;
        writer.execute_batch("BEGIN; INSERT INTO test VALUES(1)").unwrap();
        assert!(owners(&registry().lock().unwrap(), waiter_id).contains("writer conn="));
        writer.execute_batch(include_str!("sql/database_diagnostics/fingerprint_rollback.sql")).unwrap();
        assert!(registry().lock().unwrap()[&writer_id].writer.is_none());
        drop(writer);
        assert!(!registry().lock().unwrap().contains_key(&writer_id));
    }

    #[test]
    fn fingerprints_do_not_expose_sql_values() {
        let result = fingerprint(b"INSERT INTO secrets VALUES ('private-token')");
        assert!(result.to_string().starts_with("INSERT#"));
        assert!(!result.to_string().contains("private"));
        assert_ne!(result, fingerprint(b"SELECT 1"));
    }

    #[test]
    fn contending_writer_is_identified_and_waiter_resumes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("contention.db");
        let writer = Connection::open(&path).unwrap();
        writer.execute_batch(include_str!("sql/database_diagnostics/contending_writer_is_identified_and_waiter_resumes_pragma.sql")).unwrap();
        install(&writer);
        writer.execute_batch("BEGIN; INSERT INTO test VALUES(1)").unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let waiter = Connection::open(path).unwrap();
            install(&waiter);
            sender.send(unsafe { waiter.handle() } as usize).unwrap();
            waiter.execute(include_str!("sql/database_diagnostics/contending_writer_is_identified_and_waiter_resumes_insert.sql"), []).unwrap();
        });
        let waiter_id = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut observed = false;
        while Instant::now() < deadline {
            {
                let states = registry().lock().unwrap();
                if states[&waiter_id].waiting.is_some() {
                    observed = owners(&states, waiter_id).contains("writer conn=");
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        writer.execute_batch(include_str!("sql/database_diagnostics/fingerprint_commit.sql")).unwrap();
        thread.join().unwrap();
        assert!(observed, "busy callback must identify the writer before it releases its lock");
        assert_eq!(writer.query_row(include_str!("sql/database_diagnostics/contending_writer_is_identified_and_waiter_resumes_select.sql"), [], |row| row.get::<_, i64>(0)).unwrap(), 2);
    }

    #[test]
    fn busy_handler_stops_at_the_existing_thirty_second_budget() {
        let conn = Connection::open_in_memory().unwrap();
        let timeout = Duration::from_secs(30);
        conn.busy_timeout(timeout).unwrap();
        install(&conn);
        let id = unsafe { conn.handle() } as usize;
        assert_eq!(registry().lock().unwrap()[&id].timeout, timeout);
        registry().lock().unwrap().get_mut(&id).unwrap().waiting = Some(Instant::now() - timeout);
        assert_eq!(unsafe { busy(id as *mut c_void, 1) }, 0);
    }
}
