//! Drive an isolated native UI while an external sampler records memory.
//!
//! Pass an isolated app-data directory. Write one command per line to its
//! `profile-commands` file: page, trash, reader, library, trim, heap, or quit. `reader`
//! opens the ReaderSession in `profile-reader.json`. Maintenance stays paused
//! so missing book files in a metadata-only fixture do not alter its contents.
use app::{AppDataLocation, host::BackendLaunch};
use browser_ui::ReaderSession;
use gpui::{AppContext, Keystroke};
#[cfg(target_os = "linux")]
use std::ffi::{c_char, c_void};
use std::{fs, path::PathBuf, time::Duration};

#[cfg(target_os = "linux")]
#[link(name = "dl")]
unsafe extern "C" {
    fn malloc_trim(pad: usize) -> i32;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
}

#[cfg(target_os = "linux")]
fn dump_heap() {
    // A signal can interrupt an allocation while the profiler holds its lock.
    // Run dumps at a normal event-loop boundary instead.
    let symbol = unsafe { dlsym(std::ptr::null_mut(), c"HeapProfilerDump".as_ptr()) };
    assert!(!symbol.is_null(), "preload libtcmalloc to capture heap snapshots");
    let dump: unsafe extern "C" fn(*const c_char) = unsafe { std::mem::transmute(symbol) };
    unsafe { dump(c"memory-profile checkpoint".as_ptr()) };
}

fn main() {
    let directory = PathBuf::from(std::env::args().nth(1).expect("usage: memory-profile ISOLATED_APP_DATA_DIRECTORY"));
    assert!(directory.join("profile-isolated").is_file(), "create the isolation marker before profiling");
    assert!(directory.join("app-state-v1.sqlite3").is_file(), "seed the isolated registry before profiling");
    let _maintenance = app::host::defer_library_maintenance();
    app::init_logging();
    let backend = BackendLaunch::initialize(AppDataLocation::native_path(&directory)).expect("open isolated data");
    let reader: Option<ReaderSession> = fs::read(directory.join("profile-reader.json")).ok().map(|bytes| serde_json::from_slice(&bytes).expect("reader session"));
    gpui_platform::application().with_assets(ui_components::Assets).run(move |cx| {
        let root = desktop_gpui::open_ui(backend, desktop_gpui::UiLaunchOptions::DESKTOP, None, None, cx).expect("open UI");
        let window = cx.windows()[0];
        cx.spawn(async move |cx| {
            let mut consumed = 0;
            loop {
                cx.background_executor().timer(Duration::from_millis(100)).await;
                let commands = fs::read_to_string(directory.join("profile-commands")).unwrap_or_default();
                for command in commands.lines().skip(consumed) {
                    eprintln!("memory-profile command={command}");
                    cx.update_window(window, |_, window, cx| match command {
                        "page" => {
                            window.dispatch_keystroke(Keystroke::parse("pagedown").unwrap(), cx);
                        }
                        "trash" => window.dispatch_action(Box::new(browser_ui::OpenTrash), cx),
                        "reader" => {
                            let reader = reader.as_ref().expect("provide profile-reader.json");
                            root.update(cx, |root, cx| root.open_book(reader.locator.clone(), reader.title.clone(), None, window, cx)).unwrap();
                        }
                        "library" => {
                            root.update(cx, |root, cx| root.return_to_library(window, cx)).unwrap();
                        }
                        "trim" => {
                            // Diagnostic only: distinguish free allocator pages from live UI state.
                            #[cfg(target_os = "linux")]
                            unsafe {
                                malloc_trim(0);
                            }
                        }
                        "heap" => {
                            #[cfg(target_os = "linux")]
                            dump_heap();
                        }
                        "quit" => cx.quit(),
                        _ => panic!("unknown profile command: {command}"),
                    })
                    .expect("profile window");
                }
                consumed = commands.lines().count();
            }
        })
        .detach();
    });
}
