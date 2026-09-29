//! Headless release metadata and isolated database migration entry points.
use std::path::Path;

pub fn candidate_entry() -> bool {
    let mut args = std::env::args_os().skip(1);
    let action = args.next();
    if action.as_deref() == Some(std::ffi::OsStr::new("--bokheim-update-info")) {
        println!("{}", crate::update_metadata::json().expect("invalid bundled update metadata"));
        return true;
    }
    if action.as_deref() != Some(std::ffi::OsStr::new("--bokheim-prepare-update")) {
        return false;
    }
    let result = args.next().ok_or("missing candidate job".into()).and_then(|p| linux_update_host::prepare_candidate(Path::new(&p), env!("CARGO_PKG_VERSION")));
    match result {
        Ok(()) => std::process::exit(0),
        Err(error) => {
            eprintln!("update preparation: {error}");
            std::process::exit(1)
        }
    }
}
