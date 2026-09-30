//! Exercise the installed layout in a fresh process, without test-only loading.
#![cfg(all(not(target_arch = "wasm32"), not(target_os = "android"), not(feature = "packaged-pdfium")))]

use pdf_reader_core::PdfDocumentSession;
use pdfium_render::prelude::*;
use std::{fs, process::Command};

#[test]
fn relocated_package_renders_and_searches() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("BOKHEIM_PDFIUM_PACKAGE_TEST_CHILD").is_some() {
        let directory = std::env::current_exe()?.parent().unwrap().to_path_buf();
        let session = PdfDocumentSession::open(directory.join("fixture.pdf"))?;
        assert_eq!(session.info().page_count(), 1);
        assert_eq!(session.render_page(0, Some(320))?.pixel_width(), 320);
        assert!(!session.search("Packaged PDFium")?.is_empty());
        return Ok(());
    }
    let directory = std::env::temp_dir().join(format!("pdfium-package-test-{}", std::process::id()));
    fs::create_dir_all(&directory)?;
    let executable_directory = if cfg!(target_os = "macos") { directory.join("Contents/MacOS") } else { directory.clone() };
    fs::create_dir_all(&executable_directory)?;
    #[cfg(not(windows))]
    let library_directory = if cfg!(target_os = "macos") { directory.join("Contents/Frameworks") } else { directory.join("pdfium") };
    #[cfg(not(windows))]
    fs::create_dir_all(&library_directory)?;
    let executable = executable_directory.join(if cfg!(windows) { "probe.exe" } else { "probe" });
    fs::copy(std::env::current_exe()?, &executable)?;
    #[cfg(not(windows))]
    fs::copy(env!("PDFIUM_BUILD_LIBRARY_PATH"), library_directory.join(env!("PDFIUM_LIBRARY_NAME")))?;
    {
        #[cfg(windows)]
        let pdfium = Pdfium::new(Pdfium::bind_to_statically_linked_library()?);
        #[cfg(not(windows))]
        let pdfium = Pdfium::new(Pdfium::bind_to_library(env!("PDFIUM_BUILD_LIBRARY_PATH"))?);
        let mut document = pdfium.create_new_pdf()?;
        let font = document.fonts_mut().helvetica();
        let mut page = document.pages_mut().create_page_at_end(PdfPagePaperSize::a4())?;
        page.objects_mut().create_text_object(PdfPoints::new(72.0), PdfPoints::new(720.0), "Packaged PDFium", font, PdfPoints::new(18.0))?;
        document.save_to_file(&executable_directory.join("fixture.pdf"))?;
    }
    let result =
        Command::new(&executable).args(["--exact", "relocated_package_renders_and_searches", "--nocapture"]).env("BOKHEIM_PDFIUM_PACKAGE_TEST_CHILD", "1").env_remove("BOKHEIM_PDFIUM_LIBRARY_PATH").current_dir(&directory).output()?;
    fs::remove_dir_all(&directory)?;
    assert!(result.status.success(), "{}\n{}", String::from_utf8_lossy(&result.stdout), String::from_utf8_lossy(&result.stderr));
    Ok(())
}
