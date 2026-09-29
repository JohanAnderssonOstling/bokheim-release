//! Read-only MOBI metadata/text survey. Writes extracts only to the output folder.
use std::{fs, path::Path};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().collect::<Vec<_>>();
    let out = Path::new(&args[2]);
    fs::create_dir_all(out)?;
    for (i, name) in fs::read_to_string(&args[1])?.lines().enumerate() {
        fs::write(out.join(format!("{i}.path")), name)?;
        match mobi::MobiMetadata::from_path(name) {
            Ok(meta) => {
                let lines = meta.exth.records().map(|(key, values)| format!("{key:?}\t{}", values.join(" | "))).collect::<Vec<_>>().join("\n");
                fs::write(out.join(format!("{i}.meta")), lines)?;
            }
            Err(e) => {
                eprintln!("{i}: metadata: {e}");
                continue;
            }
        }
        match mobi::Mobi::from_path(name).map(|book| book.content_as_string_lossy()) {
            Ok(text) => {
                fs::write(out.join(format!("{i}.html")), text)?;
            }
            Err(e) => eprintln!("{i}: content: {e}"),
        }
        println!("{i}: {name}");
    }
    Ok(())
}
