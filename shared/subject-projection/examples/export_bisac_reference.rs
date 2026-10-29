use std::path::PathBuf;

fn main() -> std::io::Result<()> {
    let directory = PathBuf::from(std::env::args_os().nth(1).expect("supply the web bundle directory"));
    std::fs::write(directory.join(subject_projection::bisac_reference_filename()), include_bytes!(concat!(env!("OUT_DIR"), "/bisac-reference.sqlite3")))
}
