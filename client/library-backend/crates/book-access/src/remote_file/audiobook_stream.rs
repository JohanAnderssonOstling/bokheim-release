//! Opt-in recovery for native audiobook range reads. Document readers keep the
//! ordinary single-attempt adapter. A retry never advances the file position.
use super::*;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

#[derive(Default)]
struct ControlState {
    requested_generation: u64,
    reader_generation: u64,
    cancelled: bool,
    paused: bool,
    pending: bool,
    failure: Option<String>,
}

#[derive(Clone, Default)]
pub struct AudioStreamControl(Arc<(Mutex<ControlState>, Condvar)>);

impl AudioStreamControl {
    /// Invalidate the current range without closing this source or changing its
    /// pinned revision. Only the decoder may acknowledge the requested seek.
    pub fn request_seek(&self, generation: u64) {
        let mut state = self.0 .0.lock().unwrap();
        state.requested_generation = state.requested_generation.max(generation);
        drop(state);
        self.0 .1.notify_all();
    }

    pub fn begin_seek(&self, generation: u64) -> bool {
        let mut state = self.0 .0.lock().unwrap();
        if state.cancelled || generation != state.requested_generation {
            return false;
        }
        state.reader_generation = generation;
        state.pending = false;
        true
    }

    pub fn is_interrupted(&self) -> bool {
        let state = self.0 .0.lock().unwrap();
        state.requested_generation != state.reader_generation
    }

    fn check_generation(&self, generation: u64) -> io::Result<()> {
        Self::check_state(&self.0 .0.lock().unwrap(), generation)
    }

    fn check_state(state: &ControlState, generation: u64) -> io::Result<()> {
        // Do not use Interrupted: Read::read_exact retries it internally and
        // would never give the decoder a chance to process Stop or Seek.
        if state.cancelled {
            return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "audiobook stream cancelled"));
        }
        if generation != state.requested_generation {
            return Err(io::Error::new(io::ErrorKind::WouldBlock, "audiobook stream seek requested"));
        }
        Ok(())
    }

    pub fn cancel(&self) {
        self.0 .0.lock().unwrap().cancelled = true;
        self.0 .1.notify_all();
    }

    pub fn set_paused(&self, paused: bool) {
        self.0 .0.lock().unwrap().paused = paused;
        self.0 .1.notify_all();
    }

    pub fn is_cancelled(&self) -> bool {
        self.0 .0.lock().unwrap().cancelled
    }
    pub fn is_paused(&self) -> bool {
        self.0 .0.lock().unwrap().paused
    }
    pub fn is_pending(&self) -> bool {
        self.0 .0.lock().unwrap().pending
    }
    pub fn failure(&self) -> Option<String> {
        self.0 .0.lock().unwrap().failure.clone()
    }

    fn wait_retry(&self, delay: Duration, generation: u64) -> io::Result<()> {
        let deadline = Instant::now() + delay;
        let mut state = self.0 .0.lock().unwrap();
        loop {
            Self::check_state(&state, generation)?;
            if state.paused {
                state = self.0 .1.wait(state).unwrap();
                continue;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(());
            }
            state = self.0 .1.wait_timeout(state, remaining).unwrap().0;
        }
    }

    fn complete(&self, failure: Option<String>) {
        let mut state = self.0 .0.lock().unwrap();
        state.pending = false;
        state.failure = failure;
    }

    fn io_failure(&self, message: &str) -> io::Error {
        self.complete(Some(message.to_owned()));
        io::Error::other(message.to_owned())
    }
}

#[derive(Clone, Copy)]
struct RetryPolicy {
    attempts: usize,
    initial_delay: Duration,
    max_delay: Duration,
}
impl RetryPolicy {
    fn delay(self, retry: usize) -> Duration {
        self.initial_delay.saturating_mul(1u32.checked_shl(retry.min(31) as u32).unwrap_or(u32::MAX)).min(self.max_delay)
    }
}
const RETRY: RetryPolicy = RetryPolicy { attempts: 8, initial_delay: Duration::from_millis(500), max_delay: Duration::from_secs(8) };

pub fn native_audio_reader<F>(length: u64, prefix: Vec<u8>, fetch: F) -> Result<(crate::BoxedBookReader, AudioStreamControl), String>
where
    F: FnMut(u64, usize) -> crate::FetchFuture<'static, Result<Vec<u8>, RemoteFileError>> + Send + 'static,
{
    audio_reader(length, prefix, fetch, RETRY)
}

fn audio_reader<F>(length: u64, prefix: Vec<u8>, mut fetch: F, policy: RetryPolicy) -> Result<(crate::BoxedBookReader, AudioStreamControl), String>
where
    F: FnMut(u64, usize) -> crate::FetchFuture<'static, Result<Vec<u8>, RemoteFileError>> + Send + 'static,
{
    validate_prefix(length, &prefix)?;
    let control = AudioStreamControl::default();
    let worker_control = control.clone();
    type Request = (u64, u64, usize, std::sync::mpsc::SyncSender<Result<Vec<u8>, RemoteFileError>>);
    let (send, receive) = std::sync::mpsc::sync_channel::<Request>(1);
    let worker = std::thread::Builder::new()
        .name("audiobook-ranges".into())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build();
            while let Ok((generation, offset, length, result)) = receive.recv() {
                let response = match &runtime {
                    Ok(runtime) => runtime.block_on(async {
                        let request = tokio::time::timeout(REQUEST_TIMEOUT, fetch(offset, length));
                        tokio::pin!(request);
                        loop {
                            if worker_control.check_generation(generation).is_err() {
                                break Err(RemoteFileError::Failed("audiobook stream interrupted".into()));
                            }
                            tokio::select! {
                                result = &mut request => break result.unwrap_or_else(|_| Err(RemoteFileError::Retryable("remote file read timed out".into()))),
                                _ = tokio::time::sleep(Duration::from_millis(20)) => {},
                            }
                        }
                    }),
                    Err(error) => Err(RemoteFileError::Failed(error.to_string())),
                };
                let _ = result.send(response);
            }
        })
        .map_err(|error| error.to_string())?;
    let reader_control = control.clone();
    let reader = RangeReader::new(
        length,
        Box::new(move |offset, length| {
            let generation = {
                let mut state = reader_control.0 .0.lock().unwrap();
                AudioStreamControl::check_state(&state, state.reader_generation)?;
                state.pending = true;
                state.reader_generation
            };
            for attempt in 0..policy.attempts {
                // Pause only gates retries: a normal read can fill the prebuffer
                // while paused, but a disconnected source must not retry forever.
                if attempt > 0 {
                    reader_control.wait_retry(policy.delay(attempt - 1), generation)?;
                }
                reader_control.check_generation(generation)?;
                let (reply, result) = std::sync::mpsc::sync_channel(1);
                send.send((generation, offset, length, reply)).map_err(|_| reader_control.io_failure("audiobook range worker stopped"))?;
                let response = result.recv().map_err(|_| reader_control.io_failure("audiobook range worker stopped"))?;
                let response = response.and_then(|bytes| {
                    if bytes.len() == length {
                        Ok(bytes)
                    } else if bytes.len() < length {
                        Err(RemoteFileError::Retryable("incomplete remote file range".into()))
                    } else {
                        Err(RemoteFileError::Failed("oversized remote file range".into()))
                    }
                });
                reader_control.check_generation(generation)?;
                match response {
                    Ok(bytes) => {
                        reader_control.complete(None);
                        return Ok(bytes);
                    }
                    Err(RemoteFileError::Retryable(_)) if attempt + 1 < policy.attempts => {}
                    Err(error) => {
                        let message = error.to_string();
                        reader_control.complete(Some(message.clone()));
                        return Err(io::Error::other(message));
                    }
                }
            }
            unreachable!("retry policy always allows an initial attempt")
        }),
    )
    .with_prefix(prefix);
    Ok((Box::new(AudioRangeReader { reader: Some(reader), control: control.clone(), worker: Some(worker) }), control))
}

struct AudioRangeReader {
    reader: Option<RangeReader>,
    control: AudioStreamControl,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Read for AudioRangeReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        {
            let state = self.control.0 .0.lock().unwrap();
            AudioStreamControl::check_state(&state, state.reader_generation)?;
        }
        if let Some(error) = self.control.failure() {
            return Err(io::Error::other(error));
        }
        self.reader.as_mut().unwrap().read(output)
    }
}
impl Seek for AudioRangeReader {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.reader.as_mut().unwrap().seek(position)
    }
}
impl Drop for AudioRangeReader {
    fn drop(&mut self) {
        self.control.cancel();
        self.reader.take(); // Close the request sender before joining its worker.
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    const FAST: RetryPolicy = RetryPolicy { attempts: 3, initial_delay: Duration::ZERO, max_delay: Duration::ZERO };

    #[test]
    fn retry_backoff_is_capped() {
        assert_eq!(RETRY.delay(0), Duration::from_millis(500));
        assert_eq!(RETRY.delay(1), Duration::from_secs(1));
        assert_eq!(RETRY.delay(20), Duration::from_secs(8));
    }

    #[test]
    fn temporary_failure_retries_same_range_without_advancing_position() {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let (mut reader, control) = audio_reader(
            (BLOCK_BYTES * 2) as u64,
            vec![1; BLOCK_BYTES],
            move |offset, length| {
                assert_eq!(offset, BLOCK_BYTES as u64);
                let attempt = observed.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    if attempt < 2 {
                        Err(RemoteFileError::Retryable("offline".into()))
                    } else {
                        Ok(vec![7; length])
                    }
                })
            },
            FAST,
        )
        .unwrap();
        reader.seek(SeekFrom::Start(BLOCK_BYTES as u64)).unwrap();
        let mut bytes = [0; 16];
        reader.read_exact(&mut bytes).unwrap();
        assert_eq!(bytes, [7; 16]);
        assert_eq!(reader.stream_position().unwrap(), BLOCK_BYTES as u64 + 16);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert!(control.failure().is_none());
    }

    #[test]
    fn permanent_failure_is_not_retried_or_treated_as_eof() {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let (mut reader, control) = audio_reader(
            (BLOCK_BYTES * 2) as u64,
            vec![1; BLOCK_BYTES],
            move |_, _| {
                observed.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Err(RemoteFileError::Failed("source revision changed".into())) })
            },
            FAST,
        )
        .unwrap();
        reader.seek(SeekFrom::Start(BLOCK_BYTES as u64)).unwrap();
        assert!(reader.read(&mut [0; 1]).is_err());
        assert_eq!(reader.stream_position().unwrap(), BLOCK_BYTES as u64);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(control.failure().as_deref(), Some("source revision changed"));
    }

    #[test]
    fn cancelling_interrupts_an_inflight_fetch() {
        let (started, waiting) = std::sync::mpsc::sync_channel(1);
        let (mut reader, control) = audio_reader(
            (BLOCK_BYTES * 2) as u64,
            vec![1; BLOCK_BYTES],
            move |_, _| {
                let started = started.clone();
                Box::pin(async move {
                    started.send(()).unwrap();
                    std::future::pending().await
                })
            },
            FAST,
        )
        .unwrap();
        let (finished, result) = std::sync::mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            reader.seek(SeekFrom::Start(BLOCK_BYTES as u64)).unwrap();
            finished.send(reader.read_exact(&mut [0; 1]).unwrap_err().kind()).unwrap();
        });
        waiting.recv_timeout(Duration::from_secs(2)).unwrap();
        control.cancel();
        assert_eq!(result.recv_timeout(Duration::from_secs(2)).unwrap(), io::ErrorKind::ConnectionAborted);
        thread.join().unwrap();
    }

    #[test]
    fn exhausted_retry_budget_cannot_restart_on_another_decoder_read() {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let (mut reader, control) = audio_reader(
            (BLOCK_BYTES * 2) as u64,
            vec![1; BLOCK_BYTES],
            move |_, _| {
                observed.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Err(RemoteFileError::Retryable("offline".into())) })
            },
            FAST,
        )
        .unwrap();
        reader.seek(SeekFrom::Start(BLOCK_BYTES as u64)).unwrap();
        for _ in 0..8 {
            assert!(reader.read(&mut [0; 1]).is_err());
        }
        assert_eq!(calls.load(Ordering::SeqCst), FAST.attempts);
        assert_eq!(reader.stream_position().unwrap(), BLOCK_BYTES as u64);
        assert!(control.failure().is_some());
    }

    #[test]
    fn pause_holds_retries_until_explicit_resume() {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let (attempted, attempts) = std::sync::mpsc::channel();
        let (mut reader, control) = audio_reader(
            (BLOCK_BYTES * 2) as u64,
            vec![1; BLOCK_BYTES],
            move |_, length| {
                let attempt = observed.fetch_add(1, Ordering::SeqCst);
                attempted.send(attempt).unwrap();
                Box::pin(async move {
                    if attempt == 0 {
                        Err(RemoteFileError::Retryable("offline".into()))
                    } else {
                        Ok(vec![9; length])
                    }
                })
            },
            FAST,
        )
        .unwrap();
        control.set_paused(true);
        let thread = std::thread::spawn(move || {
            reader.seek(SeekFrom::Start(BLOCK_BYTES as u64)).unwrap();
            let mut byte = [0];
            reader.read_exact(&mut byte).unwrap();
            byte
        });
        assert_eq!(attempts.recv_timeout(Duration::from_secs(2)).unwrap(), 0);
        assert!(attempts.recv_timeout(Duration::from_millis(50)).is_err());
        control.set_paused(false);
        assert_eq!(attempts.recv_timeout(Duration::from_secs(2)).unwrap(), 1);
        assert_eq!(thread.join().unwrap(), [9]);
    }

    #[test]
    fn seek_interrupts_read_exact_and_reuses_the_same_source() {
        let (started, waiting) = std::sync::mpsc::channel();
        let (mut reader, control) = audio_reader(
            (BLOCK_BYTES * 3) as u64,
            vec![1; BLOCK_BYTES],
            move |offset, length| {
                let started = started.clone();
                Box::pin(async move {
                    if offset == BLOCK_BYTES as u64 {
                        started.send(()).unwrap();
                        std::future::pending().await
                    } else {
                        assert_eq!(offset, (BLOCK_BYTES * 2) as u64);
                        Ok(vec![7; length])
                    }
                })
            },
            FAST,
        )
        .unwrap();
        let (finished, result) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            reader.seek(SeekFrom::Start(BLOCK_BYTES as u64)).unwrap();
            let error = reader.read_exact(&mut [0; 16]).unwrap_err().kind();
            finished.send((reader, error)).unwrap();
        });
        waiting.recv_timeout(Duration::from_secs(2)).unwrap();
        control.request_seek(1);
        let (mut reader, error) = result.recv_timeout(Duration::from_secs(2)).unwrap();
        thread.join().unwrap();
        assert_eq!(error, io::ErrorKind::WouldBlock);
        assert_eq!(reader.stream_position().unwrap(), BLOCK_BYTES as u64);
        assert!(!control.is_cancelled());
        assert!(control.failure().is_none());
        assert!(reader.read_exact(&mut [0; 1]).is_err(), "decoder must acknowledge the seek first");
        assert!(control.begin_seek(1));
        reader.seek(SeekFrom::Start((BLOCK_BYTES * 2) as u64)).unwrap();
        let mut bytes = [0; 16];
        reader.read_exact(&mut bytes).unwrap();
        assert_eq!(bytes, [7; 16]);
    }

    #[test]
    fn newer_seek_interrupts_paused_retry_and_rejects_stale_acknowledgement() {
        let (started, waiting) = std::sync::mpsc::channel();
        let (mut reader, control) = audio_reader(
            (BLOCK_BYTES * 2) as u64,
            vec![1; BLOCK_BYTES],
            move |_, _| {
                started.send(()).unwrap();
                Box::pin(async { Err(RemoteFileError::Retryable("offline".into())) })
            },
            FAST,
        )
        .unwrap();
        control.set_paused(true);
        let (finished, result) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            reader.seek(SeekFrom::Start(BLOCK_BYTES as u64)).unwrap();
            let error = reader.read_exact(&mut [0; 1]).unwrap_err().kind();
            finished.send((reader, error)).unwrap();
        });
        waiting.recv_timeout(Duration::from_secs(2)).unwrap();
        control.request_seek(1);
        control.request_seek(2);
        let (mut reader, error) = result.recv_timeout(Duration::from_secs(2)).unwrap();
        thread.join().unwrap();
        assert_eq!(error, io::ErrorKind::WouldBlock);
        assert!(!control.begin_seek(1));
        assert!(control.is_interrupted());
        assert!(control.begin_seek(2));
        reader.seek(SeekFrom::Start(0)).unwrap();
        let mut byte = [0];
        reader.read_exact(&mut byte).unwrap();
        assert_eq!(byte, [1]);
        assert!(control.0 .0.lock().unwrap().paused, "seeking must not resume playback");
    }
}
