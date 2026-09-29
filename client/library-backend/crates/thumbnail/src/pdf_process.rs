//! A batch owns one lazy PDFium process. Only requested byte ranges cross IPC;
//! the parent resizes and encodes returned pixels after releasing the worker.
use super::*;
use std::{
    io::{self, SeekFrom, Write},
    path::PathBuf,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::OnceLock,
};

const ARGUMENT: &str = "--bokheim-pdf-cover-worker";
const READ: u8 = 1;
const IMAGE: u8 = 2;
const ERROR: u8 = 3;
const MAX_BLOCK: usize = 64 * 1024;
const MAX_PIXELS: usize = 64 * 1024 * 1024;
static EXECUTABLE: OnceLock<PathBuf> = OnceLock::new();

pub fn configure_pdf_cover_process(executable: PathBuf) {
    let _ = EXECUTABLE.set(executable);
}

#[derive(Default)]
pub struct PdfCoverProcess {
    worker: Option<Worker>,
}
struct Worker {
    child: Child,
    input: Option<ChildStdin>,
    output: ChildStdout,
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.input.take();
        // This child belongs exclusively to this batch, including cancellation.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl PdfCoverProcess {
    pub fn extract(&mut self, mut reader: impl Read + Seek + 'static) -> ThumbnailResult<RgbImage> {
        let Some(executable) = EXECUTABLE.get() else {
            return render_pdf_cover(reader);
        };
        let length = reader.seek(SeekFrom::End(0))?;
        let mut worker = match self.worker.take() {
            Some(worker) => worker,
            None => {
                let mut child = Command::new(executable).arg(ARGUMENT).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn()?;
                let input = child.stdin.take().ok_or("cover worker stdin is missing")?;
                let output = child.stdout.take().ok_or("cover worker stdout is missing")?;
                Worker { child, input: Some(input), output }
            }
        };
        let input = worker.input.as_mut().unwrap();
        input.write_all(&length.to_le_bytes())?;
        input.flush()?;
        let result = loop {
            let mut tag = [0];
            worker.output.read_exact(&mut tag)?;
            match tag[0] {
                READ => {
                    let offset = read_u64(&mut worker.output)?;
                    let count = read_u32(&mut worker.output)? as usize;
                    if count > MAX_BLOCK || offset.checked_add(count as u64).is_none_or(|end| end > length) {
                        return Err("invalid cover worker read request".into());
                    }
                    let mut bytes = vec![0; count];
                    let read = reader.seek(SeekFrom::Start(offset)).and_then(|_| reader.read_exact(&mut bytes));
                    input.write_all(&[u8::from(read.is_ok())])?;
                    if read.is_ok() {
                        input.write_all(&bytes)?;
                    }
                    input.flush()?;
                }
                IMAGE => {
                    let width = read_u32(&mut worker.output)?;
                    let height = read_u32(&mut worker.output)?;
                    let count = (u64::from(width) * u64::from(height)).checked_mul(4).ok_or("cover raster overflow")?;
                    if width == 0 || height == 0 || count > MAX_PIXELS as u64 {
                        return Err("invalid cover raster dimensions".into());
                    }
                    let mut bytes = vec![0; count as usize];
                    worker.output.read_exact(&mut bytes)?;
                    break rgb_from_rgba(&bytes, width, height);
                }
                ERROR => {
                    let count = read_u32(&mut worker.output)? as usize;
                    if count > 16 * 1024 {
                        return Err("oversized cover worker error".into());
                    }
                    let mut bytes = vec![0; count];
                    worker.output.read_exact(&mut bytes)?;
                    break Err(String::from_utf8_lossy(&bytes).into_owned().into());
                }
                _ => return Err("invalid cover worker response".into()),
            }
        };
        self.worker = Some(worker);
        result
    }
}

fn read_u64(input: &mut impl Read) -> io::Result<u64> {
    let mut bytes = [0; 8];
    input.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}
fn read_u32(input: &mut impl Read) -> io::Result<u32> {
    let mut bytes = [0; 4];
    input.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

struct ParentReader {
    length: u64,
    position: u64,
}
impl Read for ParentReader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = bytes.len().min(MAX_BLOCK).min(self.length.saturating_sub(self.position) as usize);
        if count == 0 {
            return Ok(0);
        }
        let mut output = io::stdout().lock();
        output.write_all(&[READ])?;
        output.write_all(&self.position.to_le_bytes())?;
        output.write_all(&(count as u32).to_le_bytes())?;
        output.flush()?;
        drop(output);
        let mut input = io::stdin().lock();
        let mut status = [0];
        input.read_exact(&mut status)?;
        if status[0] != 1 {
            return Err(io::Error::other("cover source could not be read"));
        }
        input.read_exact(&mut bytes[..count])?;
        self.position += count as u64;
        Ok(count)
    }
}
impl Seek for ParentReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let position = match from {
            SeekFrom::Start(value) => i128::from(value),
            SeekFrom::Current(value) => i128::from(self.position) + i128::from(value),
            SeekFrom::End(value) => i128::from(self.length) + i128::from(value),
        };
        self.position = u64::try_from(position).map_err(io::Error::other)?;
        Ok(self.position)
    }
}

/// Call before desktop initialization. The worker opens no database or UI.
pub fn run_pdf_cover_process() -> bool {
    if std::env::args_os().nth(1).as_deref() != Some(std::ffi::OsStr::new(ARGUMENT)) {
        return false;
    }
    if let Err(error) = serve() {
        eprintln!("PDF cover worker: {error}");
    }
    true
}
fn serve() -> ThumbnailResult<()> {
    loop {
        let mut first = [0];
        if io::stdin().read(&mut first)? == 0 {
            return Ok(());
        }
        let mut rest = [0; 7];
        io::stdin().read_exact(&mut rest)?;
        let mut length = [0; 8];
        length[0] = first[0];
        length[1..].copy_from_slice(&rest);
        let result = pdf_reader_core::render_first_page_image_from_reader(ParentReader { length: u64::from_le_bytes(length), position: 0 }, THUMBNAIL_WIDTH as u16);
        let mut output = io::stdout().lock();
        match result {
            Ok(image) => {
                if image.rgba().len() > MAX_PIXELS {
                    return Err("cover raster exceeds process limit".into());
                }
                output.write_all(&[IMAGE])?;
                output.write_all(&image.pixel_width().to_le_bytes())?;
                output.write_all(&image.pixel_height().to_le_bytes())?;
                output.write_all(image.rgba())?;
            }
            Err(error) => {
                let error = error.to_string();
                let bytes = &error.as_bytes()[..error.len().min(16 * 1024)];
                output.write_all(&[ERROR])?;
                output.write_all(&(bytes.len() as u32).to_le_bytes())?;
                output.write_all(bytes)?;
            }
        }
        output.flush()?;
    }
}
