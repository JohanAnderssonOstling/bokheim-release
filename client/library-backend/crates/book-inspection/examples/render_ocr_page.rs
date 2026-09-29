#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let page: usize = args[2].parse()?;
    pdf_reader_core::inspect_page_images(std::fs::File::open(&args[1])?, &[page], |_, image| {
        let mut bytes = format!("P6\n{} {}\n255\n", image.pixel_width(), image.pixel_height()).into_bytes();
        bytes.extend(image.rgba().chunks_exact(4).flat_map(|p| p[..3].iter().copied()));
        std::fs::write(&args[3], bytes).unwrap();
        false
    })?;
    Ok(())
}
#[cfg(target_arch = "wasm32")]
fn main() {
    panic!("this diagnostic requires native file access");
}
