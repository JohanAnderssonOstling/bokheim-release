use std::io::{Seek, Write};
use std::path::Path;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let folder = std::env::args().nth(1).ok_or("usage: profile FOLDER")?;
    let folder = Path::new(&folder);
    let started = Instant::now();
    let tracks = audiobook_folder::discover(folder)?;
    println!("discover: {:?}, tracks: {}", started.elapsed(), tracks.len());

    let started = Instant::now();
    let identity = audiobook_folder::identity(&tracks)?;
    println!("identity: {:?}, hash: {identity}", started.elapsed());

    let started = Instant::now();
    let mut archive = audiobook_folder::write_archive(tempfile::tempfile()?, &tracks)?;
    println!("archive: {:?}", started.elapsed());

    let started = Instant::now();
    archive.rewind()?;
    let mut checksum = blake3::Hasher::new();
    std::io::copy(&mut archive, &mut HashWriter(&mut checksum))?;
    archive.rewind()?;
    println!("checksum: {:?}, hash: {}", started.elapsed(), checksum.finalize());

    let started = Instant::now();
    let mut duration_ms = 0;
    let mut archive = zip::ZipArchive::new(archive)?;
    for index in 0..tracks.len() {
        let mut audio = tempfile::tempfile()?;
        std::io::copy(&mut archive.by_index(index)?, &mut audio)?;
        audio.rewind()?;
        duration_ms += audiobook_folder::duration_ms(audio)?;
    }
    println!("archive entry inspection: {:?}, duration: {duration_ms} ms", started.elapsed());
    Ok(())
}

struct HashWriter<'a>(&'a mut blake3::Hasher);
impl Write for HashWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> { self.0.update(bytes); Ok(bytes.len()) }
    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}
