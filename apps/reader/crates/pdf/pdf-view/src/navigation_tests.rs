use super::*;
use pdf_reader_core::{PdfPageSource, inspect_reader_metadata};
use std::{
    io::Cursor,
    sync::{Mutex, mpsc},
    time::Duration,
};

fn four_page_pdf() -> Vec<u8> {
    let mut objects = vec!["<< /Type /Catalog /Pages 2 0 R >>", "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R 6 0 R] /Count 4 >>"];
    objects.extend(["<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Resources << >> >>"; 4]);
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
    }
    let xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes());
    bytes
}

#[gpui::test]
async fn navigation_replaces_old_geometry_window_and_ignores_obsolete_errors(cx: &mut TestAppContext) {
    struct Hook {
        calls: Mutex<Vec<usize>>,
        started: mpsc::SyncSender<()>,
        release: Mutex<mpsc::Receiver<()>>,
        fail_first: bool,
    }
    impl PdfPageSource for Hook {
        fn prepare_startup(&self) -> Result<(), String> {
            Ok(())
        }
        fn prepare_page(&self, page: usize) -> Result<(), String> {
            let first = {
                let mut calls = self.calls.lock().unwrap();
                calls.push(page);
                calls.len() == 1
            };
            if first {
                self.started.send(()).unwrap();
                self.release.lock().unwrap().recv_timeout(Duration::from_secs(10)).unwrap();
                if self.fail_first {
                    return Err("obsolete page request failed".into());
                }
            }
            Ok(())
        }
    }

    cx.executor().allow_parking();
    let bytes = four_page_pdf();
    let (_, mut metadata, _) = inspect_reader_metadata(Cursor::new(bytes.clone())).unwrap();
    metadata.skippable_pages = Some(vec![]);
    for fail_first in [false, true] {
        let (started, wait_started) = mpsc::sync_channel(1);
        let (release, wait_release) = mpsc::channel();
        let hook = Arc::new(Hook { calls: Mutex::new(Vec::new()), started, release: Mutex::new(wait_release), fail_first });
        let worker_hook = hook.clone();
        let bytes = bytes.clone();
        let metadata = metadata.clone();
        let hash = metadata.content_hash.clone();
        let session = session::load(move || PdfDocumentSession::from_reader_with_page_source("navigation", Cursor::new(bytes), &hash, Some(metadata), Some(worker_hook))).await.unwrap();
        let (view, visual) = setup(cx);
        view.update(visual, |view, cx| {
            view.info = Some(session.info().clone());
            view.session = Some(session);
            view.trim_margins = false;
            view.available_width = 600.0;
            view.available_height = 800.0;
            // Keep this test focused on geometry rather than raster completion.
            view.rendering = true;
            view.rebuild_fit_width_plan();
            view.start_geometry(vec![PageGeometryKey { page_index: 0, trim_margins: false }, PageGeometryKey { page_index: 1, trim_margins: false }], cx);
        });
        visual.run_until_parked();
        wait_started.recv_timeout(Duration::from_secs(10)).unwrap();
        view.update(visual, |view, cx| {
            view.set_page(2, cx);
            view.set_page(3, cx);
        });
        release.send(()).unwrap();
        visual
            .condition(&view, |view, _| {
                assert!(view.error.is_none(), "{:?}", view.error);
                !view.geometry_loading && view.page_geometry.contains_key(&PageGeometryKey { page_index: 3, trim_margins: false })
            })
            .await;
        assert_eq!(*hook.calls.lock().unwrap(), vec![0, 2, 3], "finish the current page, skip old page 1, then analyze only the latest window");
        visual.update(|window, _| window.remove_window());
    }
}
