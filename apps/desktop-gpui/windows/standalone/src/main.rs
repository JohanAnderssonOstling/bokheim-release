#![cfg_attr(windows, windows_subsystem = "windows")]

use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::env;
use std::error::Error;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{self, Command};
use zip::ZipArchive;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

// The packager appends a ZIP, then this fixed trailer: magic, length, SHA-256.
const MAGIC: &[u8; 16] = b"BOKHEIM-SFX-V1\0\0";
const TRAILER_SIZE: u64 = 56;
const REQUIRED: &[&str] = &[
    "Bokheim.exe", "Bokheim.ico", "bokheim-opds-webview.exe", "taxonomy.sqlite3",
    "tts/alba/en_GB-alba-medium.onnx", "tts/george/model.onnx",
];

struct Payload {
    file: File,
    offset: u64,
    length: u64,
    digest: [u8; 32],
}

impl Payload {
    fn open(path: &Path) -> Result<Self> {
        let mut file = File::open(path)?;
        let size = file.metadata()?.len();
        if size < TRAILER_SIZE { return Err("The standalone download is incomplete.".into()); }
        file.seek(SeekFrom::End(-(TRAILER_SIZE as i64)))?;
        let mut trailer = [0; TRAILER_SIZE as usize];
        file.read_exact(&mut trailer)?;
        if &trailer[..16] != MAGIC { return Err("The standalone package trailer is missing.".into()); }
        let length = u64::from_le_bytes(trailer[16..24].try_into()?);
        let offset = size.checked_sub(TRAILER_SIZE).and_then(|end| end.checked_sub(length))
            .filter(|offset| *offset >= 2 && length > 0).ok_or("Invalid standalone payload length.")?;
        Ok(Self { file, offset, length, digest: trailer[24..].try_into()? })
    }

    fn key(&self) -> String {
        self.digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn verify(&mut self) -> Result<()> {
        self.file.seek(SeekFrom::Start(self.offset))?;
        let mut source = (&mut self.file).take(self.length);
        let mut hash = Sha256::new();
        let mut buffer = [0; 64 * 1024];
        loop {
            let count = source.read(&mut buffer)?;
            if count == 0 { break; }
            hash.update(&buffer[..count]);
        }
        let digest: [u8; 32] = hash.finalize().into();
        if source.limit() != 0 || digest != self.digest {
            return Err("The standalone download is damaged. Download it again.".into());
        }
        Ok(())
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && !name.contains(['\\', ':', '\0']) && name.split('/').all(|part| {
        let base = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        !part.is_empty() && part != "." && part != ".." && !part.ends_with(['.', ' '])
            && !matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            && !(base.len() == 4 && (base.starts_with("COM") || base.starts_with("LPT"))
                && matches!(base.as_bytes()[3], b'1'..=b'9'))
    })
}

fn complete(directory: &Path) -> bool {
    REQUIRED.iter().all(|name| directory.join(name).is_file())
}

fn extract(payload: &mut Payload, directory: &Path) -> Result<()> {
    payload.verify()?;
    // ZIP offsets are relative to the ZIP itself. Limit the reader to the payload
    // so the executable prefix and trailer cannot be mistaken for archive data.
    let reader = PayloadReader { file: &mut payload.file, offset: payload.offset, length: payload.length, position: 0 };
    let mut archive = ZipArchive::new(reader)?;
    let mut names = HashSet::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let name = entry.name().trim_end_matches('/').to_owned();
        if !valid_name(&name) || entry.is_symlink() || !names.insert(name.to_ascii_lowercase()) {
            return Err(format!("Unsafe or duplicate standalone entry: {name}").into());
        }
        let path = directory.join(&name);
        if entry.is_dir() { fs::create_dir_all(path)?; continue; }
        fs::create_dir_all(path.parent().ok_or("Missing runtime file parent.")?)?;
        let mut output = OpenOptions::new().write(true).create_new(true).open(path)?;
        let bytes = io::copy(&mut entry, &mut output)?;
        if bytes != entry.size() { return Err("Incomplete standalone runtime file.".into()); }
        output.sync_all()?;
    }
    if !complete(directory) { return Err("The standalone package is missing runtime files.".into()); }
    Ok(())
}

struct PayloadReader<'a> {
    file: &'a mut File,
    offset: u64,
    length: u64,
    position: u64,
}

impl Read for PayloadReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let limit = (self.length - self.position).min(buffer.len() as u64) as usize;
        self.file.seek(SeekFrom::Start(self.offset + self.position))?;
        let count = self.file.read(&mut buffer[..limit])?;
        self.position += count as u64;
        Ok(count)
    }
}

impl Seek for PayloadReader<'_> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let position = match from {
            SeekFrom::Start(position) => i128::from(position),
            SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
            SeekFrom::End(offset) => i128::from(self.length) + i128::from(offset),
        };
        if position < 0 || position > i128::from(self.length) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Seek outside standalone payload"));
        }
        self.position = position as u64;
        Ok(self.position)
    }
}

fn prepare(executable: &Path, cache: &Path) -> Result<PathBuf> {
    let mut payload = Payload::open(executable)?;
    let key = payload.key();
    fs::create_dir_all(cache)?;
    let lock = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(cache.join(format!("{key}.lock")))?;
    // Windows releases this OS lock even if extraction crashes. Other launches
    // wait and retry preparation instead of observing a half-extracted runtime.
    lock.lock()?;
    let directory = cache.join(&key);
    if fs::read_to_string(directory.join(".ready")).is_ok_and(|marker| marker == key) {
        if !complete(&directory) { return Err("The cached Bokheim runtime is incomplete. Remove its cache folder and launch again.".into()); }
        // The application updater owns this directory after extraction. Never
        // overwrite a newer application with the version embedded in this EXE.
        return Ok(directory);
    }
    if directory.exists() { return Err("The Bokheim cache folder has no completion marker.".into()); }
    let staging = cache.join(format!("{key}.partial"));
    if staging.exists() { fs::remove_dir_all(&staging)?; }
    fs::create_dir(&staging)?;
    let result: Result<()> = (|| {
        extract(&mut payload, &staging)?;
        let mut marker = File::create(staging.join(".ready"))?;
        marker.write_all(key.as_bytes())?;
        marker.sync_all()?;
        // Close the file before renaming its containing directory on Windows.
        drop(marker);
        fs::rename(&staging, &directory)?;
        Ok(())
    })();
    if result.is_err() { let _ = fs::remove_dir_all(&staging); }
    result?;
    Ok(directory)
}

fn run() -> Result<i32> {
    let local = env::var_os("LOCALAPPDATA").ok_or("Windows LOCALAPPDATA is unavailable.")?;
    let cache = PathBuf::from(local).join("Bokheim/Standalone");
    let directory = prepare(&env::current_exe()?, &cache)?;
    // Preserve the caller's working directory so relative book paths keep their
    // meaning. Child standard handles and all command-line arguments are inherited.
    let status = Command::new(directory.join("Bokheim.exe")).args(env::args_os().skip(1)).status()?;
    Ok(status.code().unwrap_or(1))
}

fn main() {
    match run() {
        Ok(code) => process::exit(code),
        Err(error) => {
            eprintln!("Bokheim: {error}");
            #[cfg(windows)]
            {
                use std::ptr::null_mut;
                use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
                let message: Vec<u16> = format!("Could not start Bokheim.\n\n{error}\0").encode_utf16().collect();
                let title: Vec<u16> = "Bokheim\0".encode_utf16().collect();
                // Both UTF-16 strings remain alive for the synchronous dialog.
                unsafe { MessageBoxW(null_mut(), message.as_ptr(), title.as_ptr(), MB_OK | MB_ICONERROR); }
            }
            process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests;
