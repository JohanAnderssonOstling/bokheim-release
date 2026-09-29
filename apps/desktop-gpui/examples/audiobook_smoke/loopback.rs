//! Isolated HTTP fixture: real media bytes and production range/auth headers.
use std::{fs, io::{self, BufRead, BufReader, Write}, net::{TcpListener, TcpStream}, path::Path,
    sync::{Arc, Mutex, atomic::{AtomicBool, AtomicUsize, Ordering}}, thread, time::Duration};

#[derive(serde::Serialize)]
struct RangeEvent { start: usize, end: usize, failed: bool, pinned: bool }

pub(super) struct RemoteAudio {
    port: u16,
    stop: Arc<AtomicBool>,
    failures: Arc<AtomicUsize>,
    offline: Arc<AtomicBool>,
    events: Arc<Mutex<Vec<RangeEvent>>>,
    errors: Arc<Mutex<Vec<String>>>,
    server: Option<thread::JoinHandle<()>>,
}

impl RemoteAudio {
    pub fn start(directory: &Path, fixture: &super::Fixture) -> Result<Self, Box<dyn std::error::Error>> {
        if fixture.record.metadata.duration_ms < 1_200_000 { return Err("use remote-seed for this scenario".into()); }
        let origin = directory.join("origin");
        let media = fs::read(directory.join("silent.m4b"))?;
        if media.len() < 4 * 1024 * 1024 { return Err("remote fixture must exceed the range cache".into()); }
        let local = origin.join("silent.m4b");
        let withheld = directory.join("withheld-origin.m4b");
        if local.exists() {
            if withheld.exists() { return Err("refusing to overwrite withheld fixture media".into()); }
            fs::rename(&local, &withheld)?;
        }
        if !withheld.is_file() { return Err("missing imported media: fixture layout changed".into()); }
        let hash = fixture.record.locator.content_hash();
        let sql = format!("UPDATE book_dir SET is_downloaded=0,local_hash='' WHERE book_row_id=(SELECT row_id FROM book WHERE content_hash='{hash}'); SELECT changes();");
        let updated = std::process::Command::new("sqlite3").arg(origin.join(".bokheim/library.db")).arg(sql).output()?;
        if !updated.status.success() || String::from_utf8_lossy(&updated.stdout).trim() != "1" {
            return Err("could not mark the isolated book as remote".into());
        }
        fs::write(directory.join("profile/audiobook-loopback-fixture"), b"bokheim-audiobook-loopback-v1")?;
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let failures = Arc::new(AtomicUsize::new(0));
        let offline = Arc::new(AtomicBool::new(false));
        let events = Arc::new(Mutex::new(Vec::new()));
        let errors = Arc::new(Mutex::new(Vec::new()));
        let route = format!("/api/libraries/{}/blobs/{hash}", fixture.record.locator.library_id());
        let checksum = format!("\"{hash}\"");
        let server = {
            let (stop, failures, events, errors) = (stop.clone(), failures.clone(), events.clone(), errors.clone());
            let offline = offline.clone();
            thread::spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    let (socket, _) = match listener.accept() {
                        Ok(connection) => connection,
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => { thread::sleep(Duration::from_millis(5)); continue; },
                        Err(error) => { errors.lock().unwrap().push(error.to_string()); break; },
                    };
                    if let Err(error) = serve(socket, &route, &checksum, &media, &failures, &offline, &events) {
                        errors.lock().unwrap().push(error.to_string());
                    }
                }
            })
        };
        Ok(Self { port, stop, failures, offline, events, errors, server: Some(server) })
    }

    pub fn port(&self) -> u16 { self.port }
    pub fn fail_next_ranges(&self, count: usize) { self.failures.store(count, Ordering::Release); }
    pub fn set_offline(&self, offline: bool) { self.offline.store(offline, Ordering::Release); }
    pub fn failed_requests(&self) -> usize { self.events.lock().unwrap().iter().filter(|event| event.failed).count() }
    pub fn request_count(&self) -> usize { self.events.lock().unwrap().len() }
    pub fn save_trace(&self, directory: &Path) {
        let errors = self.errors.lock().unwrap();
        assert!(errors.is_empty(), "HTTP fixture errors: {errors:?}");
        fs::write(directory.join("remote-outage-ranges.json"), serde_json::to_vec_pretty(&*self.events.lock().unwrap()).unwrap()).unwrap();
    }

    pub fn verify(&self, directory: &Path) {
        assert!(self.errors.lock().unwrap().is_empty(), "HTTP fixture errors: {:?}", self.errors.lock().unwrap());
        let events = self.events.lock().unwrap();
        fs::write(directory.join("remote-ranges.json"), serde_json::to_vec_pretty(&*events).unwrap()).unwrap();
        assert!(events.len() >= 4, "playback did not use the HTTP range source");
        let failed: Vec<_> = events.iter().enumerate().filter(|(_, event)| event.failed).collect();
        assert_eq!(failed.len(), 2, "recovery must consume both injected transient failures");
        assert!(failed.iter().all(|(_, event)| event.pinned));
        let (index, last) = failed[1];
        assert!(events[index + 1..].iter().any(|event| !event.failed && event.pinned && event.start == last.start && event.end == last.end),
            "failed range was not retried with the same revision and bounds");
        assert!(!directory.join("origin/silent.m4b").exists(), "streaming installed a local book");
    }
}

impl Drop for RemoteAudio {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(server) = self.server.take() { let _ = server.join(); }
    }
}

fn serve(socket: TcpStream, route: &str, checksum: &str, media: &[u8], failures: &AtomicUsize, offline: &AtomicBool, events: &Mutex<Vec<RangeEvent>>) -> io::Result<()> {
    socket.set_read_timeout(Some(Duration::from_secs(2)))?;
    socket.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut reader = BufReader::new(socket);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let request: Vec<_> = line.split_whitespace().collect();
    let is_media = request.get(1) == Some(&route);
    let mut headers = std::collections::HashMap::new();
    let mut size = line.len();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 || line == "\r\n" { break; }
        size += line.len();
        if size > 16_384 { return Err(io::Error::other("oversized fixture request")); }
        if let Some((key, value)) = line.split_once(':') { headers.insert(key.to_ascii_lowercase(), value.trim().to_owned()); }
    }
    let mut socket = reader.into_inner();
    if !is_media { return socket.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"); }
    if headers.get("authorization") != Some(&format!("Bearer {}", "a".repeat(43))) { return Err(io::Error::other("missing/wrong fixture authorization")); }
    let range = headers.get("range").and_then(|range| range.strip_prefix("bytes=")).and_then(|range| range.split_once('-')).ok_or_else(|| io::Error::other("missing byte range"))?;
    let start: usize = range.0.parse().map_err(io::Error::other)?;
    let end: usize = range.1.parse::<usize>().map_err(io::Error::other)?.min(media.len() - 1);
    if start > end { return Err(io::Error::other("invalid requested range")); }
    let pinned = headers.get("if-range").is_some_and(|value| value == checksum);
    if headers.contains_key("if-range") && !pinned { return Err(io::Error::other("wrong source revision")); }
    let failed = offline.load(Ordering::Acquire) || failures.fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| count.checked_sub(1)).is_ok();
    events.lock().unwrap().push(RangeEvent { start, end, failed, pinned });
    if failed { return socket.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"); }
    write!(socket, "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{end}/{}\r\nETag: {checksum}\r\nConnection: close\r\n\r\n", end - start + 1, media.len())?;
    socket.write_all(&media[start..=end])
}
