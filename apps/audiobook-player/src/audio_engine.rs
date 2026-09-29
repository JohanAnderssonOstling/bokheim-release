use std::io::{BufReader, SeekFrom};
#[cfg(feature = "kobo")]
use std::io::{Read, Write};
use std::num::{NonZeroU16, NonZeroU32};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, SendTimeoutError, Sender, TryRecvError, bounded, unbounded};
use rodio::Decoder;
use rodio::source::Source;
#[cfg(not(feature = "kobo"))]
use rodio::{DeviceSinkBuilder, MixerDeviceSink, Player};
use wsola::TimeStretch;

use crate::ReadSeek;

type AudioDecoder = Decoder<BufReader<Box<dyn ReadSeek>>>;

const BLOCK_SAMPLES: usize = 16_384;
// Keep several seconds of decoded audio available so short Kobo CPU or
// storage stalls do not reach the Bluetooth output. Startup still waits only
// for KOBO_PREBUFFER_BLOCKS below, so this does not add startup latency.
const BUFFERED_BLOCKS: usize = 32;
#[cfg(feature = "kobo")]
const KOBO_PREBUFFER_BLOCKS: usize = 6;
#[cfg(feature = "kobo")]
const KOBO_PREBUFFER_TIMEOUT: Duration = Duration::from_secs(3);
const EOF_CONFIRMATION_READS: usize = 8;
const NO_GENERATION: u64 = u64::MAX;
const REALTIME_TEMPO_EPSILON: f32 = 0.0001;

enum DecoderRequest {
    Seek { position: Duration, generation: u64 },
    Tempo(f32),
    Stop,
}

#[derive(Default)]
struct PendingDecoderRequests {
    seek: Option<(Duration, u64)>,
    tempo: Option<f32>,
    stop: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DecoderRequestOutcome {
    Continue,
    Seeked,
    Stop,
}

struct AudioChunk {
    generation: u64,
    tempo: f32,
    samples: Vec<f32>,
}

struct SharedState {
    buffering: AtomicBool,
    output_lost: AtomicBool,
    stream_control: Option<app::AudioStreamControl>,
    decoder_stopped: AtomicBool,
    resume_blocks: usize,
    generation: AtomicU64,
    position: Mutex<PositionState>,
    confirmed_position: Mutex<PositionState>,
    decoder_end_generation: AtomicU64,
    playback_finished_generation: AtomicU64,
    error: Mutex<Option<String>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PositionState {
    generation: u64,
    micros: u64,
}

impl SharedState {
    fn position_micros(&self) -> u64 {
        self.position.lock().expect("audio position lock poisoned").micros
    }

    fn confirmed_position_micros(&self) -> Option<u64> {
        let position = self.position.lock().expect("audio position lock poisoned");
        let confirmed = *self.confirmed_position.lock().expect("confirmed audio position lock poisoned");
        (position.generation == confirmed.generation).then_some(confirmed.micros)
    }

    fn set_position_if_generation(&self, generation: u64, micros: u64) -> bool {
        let mut position = self.position.lock().expect("audio position lock poisoned");
        if position.generation != generation {
            return false;
        }
        position.micros = micros;
        *self.confirmed_position.lock().expect("confirmed audio position lock poisoned") = PositionState { generation, micros };
        true
    }

    fn confirm_seek(&self, generation: u64, position: Duration) {
        *self.confirmed_position.lock().expect("confirmed audio position lock poisoned") = PositionState { generation, micros: position.as_micros() as u64 };
    }

    fn reject_seek_if_current(&self, generation: u64, error: String) -> bool {
        let mut position = self.position.lock().expect("audio position lock poisoned");
        let confirmed = *self.confirmed_position.lock().expect("confirmed audio position lock poisoned");
        if self.generation.compare_exchange(generation, confirmed.generation, Ordering::AcqRel, Ordering::Acquire).is_err() {
            return false;
        }
        if position.generation == generation {
            *position = confirmed;
        }
        drop(position);
        *self.error.lock().expect("audio error lock poisoned") = Some(error);
        true
    }

    fn playback_finished(&self) -> bool {
        self.playback_finished_generation.load(Ordering::Acquire) == self.generation.load(Ordering::Acquire)
    }
}

pub struct AudioEngine {
    #[cfg(feature = "kobo")]
    recovery: Mutex<Option<OutputRecovery>>,
    stream_control: Option<app::AudioStreamControl>,
    #[cfg(not(feature = "kobo"))]
    _output: MixerDeviceSink,
    #[cfg(not(feature = "kobo"))]
    player: Player,
    #[cfg(feature = "kobo")]
    output: KoboOutput,
    #[cfg(feature = "kobo")]
    retry_output: (Receiver<AudioChunk>, NonZeroU16, NonZeroU32),
    request_tx: Sender<DecoderRequest>,
    worker: Option<JoinHandle<()>>,
    state: Arc<SharedState>,
    duration: Duration,
}

impl AudioEngine {
    pub fn open(reader: Box<dyn ReadSeek>, duration: Duration, stream_control: Option<app::AudioStreamControl>) -> Result<Self, String> {
        let _opening = crate::OpeningTiming::new("audio_engine_open");
        let decoder_timing = crate::OpeningTiming::new("decoder_init");
        let decoder = open_decoder(reader)?;
        drop(decoder_timing);
        let channels = decoder.channels();
        let sample_rate = decoder.sample_rate();

        #[cfg(not(feature = "kobo"))]
        let output = DeviceSinkBuilder::open_default_sink().map_err(|error| error.to_string())?;
        #[cfg(not(feature = "kobo"))]
        let player = Player::connect_new(output.mixer());
        #[cfg(not(feature = "kobo"))]
        player.pause();
        // UI seeks must never block behind decoder work. The worker drains all
        // pending requests before decoding its next block, so bursts naturally
        // converge on the most recent seek.
        let (request_tx, request_rx) = unbounded();
        let (audio_tx, audio_rx) = bounded(BUFFERED_BLOCKS);
        let state = Arc::new(SharedState {
            buffering: AtomicBool::new(false),
            output_lost: AtomicBool::new(false),
            stream_control: stream_control.clone(),
            decoder_stopped: AtomicBool::new(false),
            resume_blocks: if stream_control.is_some() { 6 } else { 1 },
            generation: AtomicU64::new(0),
            position: Mutex::new(PositionState { generation: 0, micros: 0 }),
            confirmed_position: Mutex::new(PositionState { generation: 0, micros: 0 }),
            decoder_end_generation: AtomicU64::new(NO_GENERATION),
            playback_finished_generation: AtomicU64::new(NO_GENERATION),
            error: Mutex::new(None),
        });
        let worker_state = state.clone();
        let decoder_state = state.clone();
        let decoder_stream = stream_control.clone();
        let worker = thread::Builder::new()
            .name("m4b-decode-wsola".into())
            .spawn(move || {
                if let Err(error) = decode_worker(decoder, channels, sample_rate, request_rx, audio_tx, decoder_state, decoder_stream) {
                    *worker_state.error.lock().expect("audio error lock poisoned") = Some(error);
                }
                worker_state.decoder_stopped.store(true, Ordering::Release);
            })
            .map_err(|error| error.to_string())?;

        #[cfg(not(feature = "kobo"))]
        player.append(WorkerSource::new(audio_rx, state.clone(), channels, sample_rate, duration));
        #[cfg(feature = "kobo")]
        let output = KoboOutput::spawn(audio_rx.clone(), state.clone(), channels, sample_rate, duration)?;

        Ok(Self {
            #[cfg(feature = "kobo")]
            recovery: Mutex::new(None),
            stream_control,
            #[cfg(not(feature = "kobo"))]
            _output: output,
            #[cfg(not(feature = "kobo"))]
            player,
            #[cfg(feature = "kobo")]
            output,
            #[cfg(feature = "kobo")]
            retry_output: (audio_rx, channels, sample_rate),
            request_tx,
            worker: Some(worker),
            state,
            duration,
        })
    }

    pub fn play(&self) {
        self.state.output_lost.store(false, Ordering::Release);
        if let Some(control) = &self.stream_control {
            control.set_paused(false);
        }
        #[cfg(not(feature = "kobo"))]
        self.player.play();
        #[cfg(feature = "kobo")]
        self.output.play();
    }

    pub fn position(&self) -> Duration {
        Duration::from_micros(self.state.position_micros())
    }

    pub fn is_buffering(&self) -> bool {
        #[cfg(feature = "kobo")]
        if self.recovery.lock().expect("audio recovery lock poisoned").is_some() {
            return true;
        }
        if !self.state.buffering.load(Ordering::Acquire) {
            return false;
        }
        // Not `is_playing()`: right after a seek the sink is legitimately
        // empty until the decoder's fresh samples arrive, which made
        // `is_playing()` — and so buffering, defined in terms of it — read
        // false for exactly the window buffering describes, leaving the
        // transport stuck showing "paused" while genuinely refilling.
        #[cfg(not(feature = "kobo"))]
        {
            !self.player.is_paused() && !self.state.playback_finished()
        }
        #[cfg(feature = "kobo")]
        {
            self.output.is_playing() && !self.state.playback_finished()
        }
    }

    pub fn take_output_lost(&self) -> bool {
        self.state.output_lost.swap(false, Ordering::AcqRel)
    }

    pub fn confirmed_position(&self) -> Option<Duration> {
        self.state.confirmed_position_micros().map(Duration::from_micros)
    }

    pub fn last_confirmed_position(&self) -> Duration {
        Duration::from_micros(self.state.confirmed_position.lock().expect("confirmed audio position lock poisoned").micros)
    }

    pub fn pause(&self) {
        #[cfg(feature = "kobo")]
        self.recovery.lock().expect("audio recovery lock poisoned").take();
        if let Some(control) = &self.stream_control {
            control.set_paused(true);
        }
        #[cfg(not(feature = "kobo"))]
        self.player.pause();
        #[cfg(feature = "kobo")]
        self.output.playing.store(false, Ordering::Release);
    }

    pub fn is_playing(&self) -> bool {
        #[cfg(not(feature = "kobo"))]
        {
            !self.player.is_paused() && !self.state.playback_finished() && !self.player.empty()
        }
        #[cfg(feature = "kobo")]
        {
            self.output.is_playing() && !self.state.playback_finished()
        }
    }

    pub fn finished(&self) -> bool {
        self.state.playback_finished()
    }

    pub fn toggle(&mut self) -> Result<(), String> {
        #[cfg(feature = "kobo")]
        if self.state.playback_finished() || self.output.thread.as_ref().is_some_and(|thread| thread.is_finished()) {
            let (receiver, channels, sample_rate) = &self.retry_output;
            let (receiver, state, channels, sample_rate, duration) = (receiver.clone(), self.state.clone(), *channels, *sample_rate, self.duration);
            let recovery = OutputRecovery::start(move |cancelled| KoboOutput::spawn_cancellable(receiver, state, channels, sample_rate, duration, || cancelled.load(Ordering::Acquire)))?;
            self.state.output_lost.store(false, Ordering::Release);
            if let Some(control) = &self.stream_control {
                control.set_paused(true);
            }
            *self.recovery.lock().expect("audio recovery lock poisoned") = Some(recovery);
            return Ok(());
        }
        if self.state.playback_finished() {
            self.seek_to(Duration::ZERO)?;
            self.play();
            return Ok(());
        }
        #[cfg(not(feature = "kobo"))]
        if self.player.is_paused() {
            self.player.play();
        } else {
            self.player.pause();
        }
        #[cfg(feature = "kobo")]
        self.output.toggle();
        if let Some(control) = &self.stream_control {
            control.set_paused(!self.is_playing());
        }
        if self.is_playing() {
            self.state.output_lost.store(false, Ordering::Release);
        }
        Ok(())
    }

    #[cfg(feature = "kobo")]
    fn restart_output(&mut self, output: KoboOutput) -> Result<(), String> {
        // The failed output dropped its partially consumed chunk. Reusing the
        // remaining queue would skip that audio while reporting an unbroken clock.
        // Invalidate old chunks and regenerate from the last confirmed position.
        let position = if self.state.playback_finished() { Duration::ZERO } else { self.confirmed_position().unwrap_or_else(|| self.position()) };
        // Stop the old output before clearing the finished generation: it may
        // still be draining aplay, or about to observe the end-of-book flag.
        self.output = output;
        self.seek_to(position)?;
        self.play();
        Ok(())
    }

    /// Called by the session refresh; command execution never runs on the UI thread.
    pub fn refresh_output(&mut self) {
        #[cfg(feature = "kobo")]
        {
            let result = {
                let mut recovery = self.recovery.lock().expect("audio recovery lock poisoned");
                let Some(pending) = recovery.as_ref() else {
                    return;
                };
                match pending.result.try_recv() {
                    Ok(result) => {
                        recovery.take();
                        result
                    }
                    Err(TryRecvError::Empty) => return,
                    Err(TryRecvError::Disconnected) => {
                        recovery.take();
                        Err("Audio output recovery stopped".to_owned())
                    }
                }
            };
            if let Err(error) = result.and_then(|output| self.restart_output(output)) {
                *self.state.error.lock().expect("audio error lock poisoned") = Some(error);
            }
        }
    }

    pub fn seek_to(&self, position: Duration) -> Result<(), String> {
        let position = position.min(self.duration);
        request_seek(&self.state, &self.request_tx, position)
    }

    pub fn set_tempo(&mut self, tempo: f32) -> Result<(), String> {
        let tempo = normalized_tempo(tempo)?;
        self.request_tx.send(DecoderRequest::Tempo(tempo)).map_err(|_| "audio decoder stopped".to_string())
    }

    pub fn set_volume(&self, volume: f32) {
        #[cfg(not(feature = "kobo"))]
        self.player.set_volume(volume);
        #[cfg(feature = "kobo")]
        self.output.set_volume(volume);
    }

    pub fn take_error(&self) -> Option<String> {
        self.state.error.lock().ok()?.take()
    }
}

fn open_decoder(mut reader: Box<dyn ReadSeek>) -> Result<AudioDecoder, String> {
    let length_timing = crate::OpeningTiming::new("source_length_probe");
    let byte_len = stream_byte_len(&mut *reader)?;
    drop(length_timing);
    Decoder::builder().with_data(BufReader::new(reader)).with_byte_len(byte_len).with_seekable(true).build().map_err(|error| error.to_string())
}

fn stream_byte_len(reader: &mut dyn ReadSeek) -> Result<u64, String> {
    let original_position = reader.stream_position().map_err(|error| error.to_string())?;
    let byte_len = reader.seek(SeekFrom::End(0)).map_err(|error| error.to_string())?;
    reader.seek(SeekFrom::Start(original_position)).map_err(|error| error.to_string())?;
    Ok(byte_len)
}

fn request_seek(state: &SharedState, request_tx: &Sender<DecoderRequest>, position: Duration) -> Result<(), String> {
    let mut current_position = state.position.lock().expect("audio position lock poisoned");
    let previous_generation = state.generation.load(Ordering::Acquire);
    let generation = previous_generation.checked_add(1).filter(|generation| *generation != NO_GENERATION).ok_or_else(|| "audio seek generation exhausted".to_owned())?;
    let previous_position = *current_position;
    state.generation.store(generation, Ordering::Release);
    *current_position = PositionState { generation, micros: position.as_micros() as u64 };
    drop(current_position);
    if let Some(control) = &state.stream_control {
        control.request_seek(generation);
    }
    if request_tx.send(DecoderRequest::Seek { position, generation }).is_ok() {
        return Ok(());
    }

    let mut current = state.position.lock().expect("audio position lock poisoned");
    if state.generation.load(Ordering::Acquire) == generation {
        state.generation.store(previous_generation, Ordering::Release);
        if current.generation == generation {
            *current = previous_position;
        }
    }
    Err("audio decoder stopped".to_string())
}

fn normalized_tempo(tempo: f32) -> Result<f32, String> {
    if !tempo.is_finite() {
        return Err("playback rate must be finite".to_owned());
    }
    Ok(tempo.clamp(wsola::MIN_TEMPO, wsola::MAX_TEMPO))
}

fn collect_pending_requests(first: Option<DecoderRequest>, request_rx: &Receiver<DecoderRequest>) -> PendingDecoderRequests {
    let mut pending = PendingDecoderRequests::default();
    let mut collect = |request| match request {
        DecoderRequest::Seek { position, generation } => pending.seek = Some((position, generation)),
        DecoderRequest::Tempo(tempo) => pending.tempo = Some(tempo),
        DecoderRequest::Stop => pending.stop = true,
    };
    if let Some(first) = first {
        collect(first);
    }
    while let Ok(request) = request_rx.try_recv() {
        collect(request);
    }
    pending
}

fn apply_pending_requests(pending: PendingDecoderRequests, decoder: &mut AudioDecoder, stretch: &mut TimeStretch, tempo: &mut f32, generation: &mut u64, state: &SharedState) -> DecoderRequestOutcome {
    if pending.stop {
        return DecoderRequestOutcome::Stop;
    }
    if let Some(next) = pending.tempo {
        *tempo = next;
        // A tempo change back to normal playback switches the decoder to the
        // direct path below. Reset WSOLA so samples buffered while stretching
        // cannot leak into the direct stream. Conversely, reset before
        // entering WSOLA so it never consumes stale state from direct audio.
        stretch.reset();
        stretch.set_tempo(next);
    }
    if let Some((position, next)) = pending.seek {
        if state.stream_control.as_ref().is_some_and(|control| !control.begin_seek(next)) {
            return DecoderRequestOutcome::Continue;
        }
        let _seek_timing = crate::OpeningTiming::new("decoder_seek");
        eprintln!("AUDIOBOOK_TIMING phase=decoder_seek generation={next} target_ms={}", position.as_millis());
        if let Err(error) = decoder.try_seek(position) {
            state.reject_seek_if_current(next, format!("Could not seek audiobook: {error}"));
            return DecoderRequestOutcome::Continue;
        }
        stretch.reset();
        *generation = next;
        state.confirm_seek(next, position);
        state.decoder_end_generation.store(NO_GENERATION, Ordering::Release);
        return DecoderRequestOutcome::Seeked;
    }
    DecoderRequestOutcome::Continue
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        #[cfg(feature = "kobo")]
        self.recovery.lock().expect("audio recovery lock poisoned").take();
        if let Some(control) = &self.stream_control {
            control.cancel();
        }
        let _ = self.request_tx.send(DecoderRequest::Stop);
        #[cfg(not(feature = "kobo"))]
        self.player.stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn read_decoder_block(decoder: &mut impl Iterator<Item = f32>, block_samples: usize) -> Vec<f32> {
    for _ in 0..EOF_CONFIRMATION_READS {
        let samples = decoder.by_ref().take(block_samples).collect::<Vec<_>>();
        if !samples.is_empty() {
            return samples;
        }
    }
    Vec::new()
}

fn decode_worker(
    mut decoder: AudioDecoder, channels: NonZeroU16, sample_rate: NonZeroU32, request_rx: Receiver<DecoderRequest>, audio_tx: Sender<AudioChunk>, state: Arc<SharedState>, stream: Option<app::AudioStreamControl>,
) -> Result<(), String> {
    let mut stretch = TimeStretch::new(sample_rate.get(), channels.get()).map_err(|error| error.to_string())?;
    let mut tempo = 1.0;
    let mut generation = 0;
    let aligned_block = BLOCK_SAMPLES / channels.get() as usize * channels.get() as usize;

    'decode: loop {
        // A seek can invalidate I/O just before its command is enqueued. Wait
        // for that command rather than spinning the decoder on WouldBlock.
        let first = if stream.as_ref().is_some_and(|control| control.is_interrupted()) {
            match request_rx.recv_timeout(Duration::from_millis(20)) {
                Ok(request) => Some(request),
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            }
        } else {
            None
        };
        let pending = collect_pending_requests(first, &request_rx);
        if apply_pending_requests(pending, &mut decoder, &mut stretch, &mut tempo, &mut generation, &state) == DecoderRequestOutcome::Stop {
            break;
        }

        // Symphonia's iterator uses `None` for both real EOF and some packet or
        // demuxer errors from which a later call can recover. A single empty
        // read therefore must not send playback to the end of the book.
        let input = read_decoder_block(&mut decoder, aligned_block);
        if let Some(control) = &stream {
            if control.is_cancelled() {
                break;
            }
            if control.is_interrupted() {
                continue;
            }
            if let Some(error) = control.failure() {
                return Err(format!("Audiobook stream unavailable: {error}"));
            }
        }
        let at_end = input.is_empty();
        let samples = if is_realtime_tempo(tempo) {
            // WSOLA is unnecessary at 1.0x and is expensive on Kobo's single
            // Cortex-A9 core. Pass decoded samples through unchanged so the
            // decoder can stay ahead of the Bluetooth output clock.
            input
        } else if at_end {
            stretch.flush()
        } else {
            let aligned = input.len() / channels.get() as usize * channels.get() as usize;
            stretch.push(&input[..aligned]);
            stretch.pull(aligned_block)
        };

        if !samples.is_empty() {
            let mut chunk = AudioChunk { generation, tempo, samples };
            loop {
                match audio_tx.send_timeout(chunk, Duration::from_millis(10)) {
                    Ok(()) => break,
                    Err(SendTimeoutError::Disconnected(_)) => break 'decode,
                    Err(SendTimeoutError::Timeout(returned)) => {
                        chunk = returned;
                        match apply_pending_requests(collect_pending_requests(None, &request_rx), &mut decoder, &mut stretch, &mut tempo, &mut generation, &state) {
                            DecoderRequestOutcome::Stop => break 'decode,
                            DecoderRequestOutcome::Seeked => continue 'decode,
                            DecoderRequestOutcome::Continue => {}
                        }
                    }
                }
            }
        }
        if at_end {
            state.decoder_end_generation.store(generation, Ordering::Release);
            loop {
                let Ok(request) = request_rx.recv() else { break 'decode };
                match apply_pending_requests(collect_pending_requests(Some(request), &request_rx), &mut decoder, &mut stretch, &mut tempo, &mut generation, &state) {
                    DecoderRequestOutcome::Stop => break 'decode,
                    DecoderRequestOutcome::Seeked => continue 'decode,
                    DecoderRequestOutcome::Continue => {}
                }
            }
        }
    }
    Ok(())
}

fn is_realtime_tempo(tempo: f32) -> bool {
    (tempo - 1.0).abs() <= REALTIME_TEMPO_EPSILON
}

struct WorkerSource {
    audio_rx: Receiver<AudioChunk>,
    state: Arc<SharedState>,
    current: Vec<f32>,
    index: usize,
    current_generation: Option<u64>,
    chunk_tempo: f32,
    channels: NonZeroU16,
    sample_rate: NonZeroU32,
    total_duration: Duration,
    frames_since_update: u32,
    source_position_us: f64,
    disconnected: bool,
}

impl WorkerSource {
    fn new(audio_rx: Receiver<AudioChunk>, state: Arc<SharedState>, channels: NonZeroU16, sample_rate: NonZeroU32, total_duration: Duration) -> Self {
        Self { audio_rx, state, current: Vec::new(), index: 0, current_generation: None, chunk_tempo: 1.0, channels, sample_rate, total_duration, frames_since_update: 0, source_position_us: 0.0, disconnected: false }
    }

    fn next_chunk(&mut self) {
        loop {
            match self.audio_rx.try_recv() {
                Ok(chunk) if chunk.generation == self.state.generation.load(Ordering::Acquire) => {
                    self.state.buffering.store(false, Ordering::Release);
                    if self.current_generation != Some(chunk.generation) {
                        self.source_position_us = self.state.position_micros() as f64;
                        self.frames_since_update = 0;
                    }
                    self.current = chunk.samples;
                    self.index = 0;
                    self.current_generation = Some(chunk.generation);
                    self.chunk_tempo = chunk.tempo;
                    return;
                }
                Ok(_) => continue,
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    self.disconnected = true;
                    return;
                }
            }
        }
    }
}

impl Iterator for WorkerSource {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        // A seek changes the generation before the decoder has produced audio
        // at the new position. Drop the currently-playing old chunk immediately
        // so that it cannot overwrite the requested position in SharedState.
        if self.current_generation.is_some_and(|generation| generation != self.state.generation.load(Ordering::Acquire)) {
            self.current.clear();
            self.index = 0;
        }
        if self.index >= self.current.len() {
            // Refill a remote stream before leaving buffering. Short tails and
            // terminal decoder failures must still drain whatever is queued.
            if self.state.buffering.load(Ordering::Acquire)
                && self.audio_rx.len() < self.state.resume_blocks
                && self.state.decoder_end_generation.load(Ordering::Acquire) == NO_GENERATION
                && !self.state.decoder_stopped.load(Ordering::Acquire)
                && self.state.resume_blocks > 1
            {
                return Some(0.0);
            }
            self.next_chunk();
        }
        if self.index >= self.current.len() {
            if self.disconnected {
                self.state.buffering.store(false, Ordering::Release);
                return None;
            }
            let end_generation = self.state.decoder_end_generation.load(Ordering::Acquire);
            self.state.buffering.store(end_generation == NO_GENERATION, Ordering::Release);
            if end_generation != NO_GENERATION && self.state.set_position_if_generation(end_generation, self.total_duration.as_micros() as u64) {
                self.state.playback_finished_generation.store(end_generation, Ordering::Release);
            }
            return Some(0.0);
        }

        let sample = self.current[self.index];
        self.index += 1;
        if self.index.is_multiple_of(self.channels.get() as usize) {
            self.source_position_us += self.chunk_tempo as f64 * 1_000_000.0 / self.sample_rate.get() as f64;
            self.frames_since_update += 1;
            if self.frames_since_update >= 256 {
                if let Some(generation) = self.current_generation {
                    let position = (self.source_position_us as u64).min(self.total_duration.as_micros() as u64);
                    self.state.set_position_if_generation(generation, position);
                }
                self.frames_since_update = 0;
            }
        }
        Some(sample)
    }
}

impl Source for WorkerSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> NonZeroU16 {
        self.channels
    }

    fn sample_rate(&self) -> NonZeroU32 {
        self.sample_rate
    }

    fn total_duration(&self) -> Option<Duration> {
        Some(self.total_duration)
    }
}

#[cfg(feature = "kobo")]
struct OutputRecovery {
    cancelled: Arc<AtomicBool>,
    result: Receiver<Result<KoboOutput, String>>,
}

#[cfg(feature = "kobo")]
impl OutputRecovery {
    fn start(work: impl FnOnce(Arc<AtomicBool>) -> Result<KoboOutput, String> + Send + 'static) -> Result<Self, String> {
        let cancelled = Arc::new(AtomicBool::new(false));
        let token = cancelled.clone();
        let (result, receiver) = bounded(1);
        thread::Builder::new()
            .name("kobo-output-recovery".into())
            .spawn(move || {
                let output = work(token.clone());
                if !token.load(Ordering::Acquire) {
                    let _ = result.send(output);
                }
            })
            .map_err(|error| format!("Could not start audio recovery: {error}"))?;
        Ok(Self { cancelled, result: receiver })
    }
}

#[cfg(feature = "kobo")]
impl Drop for OutputRecovery {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

#[cfg(feature = "kobo")]
struct KoboOutput {
    playing: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    volume_bits: Arc<AtomicU64>,
    child: Arc<Mutex<std::process::Child>>,
    thread: Option<JoinHandle<()>>,
}

#[cfg(feature = "kobo")]
impl KoboOutput {
    fn spawn(audio_rx: Receiver<AudioChunk>, state: Arc<SharedState>, channels: NonZeroU16, sample_rate: NonZeroU32, duration: Duration) -> Result<Self, String> {
        let control = state.stream_control.clone();
        Self::spawn_cancellable(audio_rx, state, channels, sample_rate, duration, || control.as_ref().is_some_and(|control| control.is_cancelled()))
    }

    fn spawn_cancellable(audio_rx: Receiver<AudioChunk>, state: Arc<SharedState>, channels: NonZeroU16, sample_rate: NonZeroU32, duration: Duration, cancelled: impl Fn() -> bool) -> Result<Self, String> {
        let channel_count = channels.get().to_string();
        let sample_rate_value = sample_rate.get();
        let sample_rate = sample_rate_value.to_string();
        let bluetooth_timing = crate::OpeningTiming::new("bluetooth_output_lookup");
        let device = connected_audio_device(&cancelled)?;
        drop(bluetooth_timing);
        if cancelled() {
            return Err("Audio output recovery cancelled".into());
        }
        let pcm = format!("bluealsa:DEV={device},PROFILE=a2dp");
        let spawn_timing = crate::OpeningTiming::new("aplay_spawn");
        let child = std::process::Command::new("aplay")
            .args(["-q", "-D", &pcm, "-t", "raw", "-f", "S16_LE", "-c", &channel_count, "-r", &sample_rate])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|error| format!("Could not start Kobo Bluetooth audio (aplay): {error}"))?;
        drop(spawn_timing);
        Self::from_child(child, audio_rx, state, channels, NonZeroU32::new(sample_rate_value).expect("sample rate is non-zero"), duration)
    }

    fn from_child(mut child: std::process::Child, audio_rx: Receiver<AudioChunk>, state: Arc<SharedState>, channels: NonZeroU16, sample_rate: NonZeroU32, duration: Duration) -> Result<Self, String> {
        let sample_rate_value = sample_rate.get();
        let mut input = child.stdin.take().ok_or_else(|| "Kobo audio player has no input pipe".to_owned())?;
        let stderr = child.stderr.take().map(|mut pipe| {
            thread::spawn(move || {
                let mut output = Vec::new();
                let _ = pipe.read_to_end(&mut output);
                output
            })
        });
        let child = Arc::new(Mutex::new(child));
        let worker_child = child.clone();
        let playing = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let volume_bits = Arc::new(AtomicU64::new(1.0f32.to_bits() as u64));
        let thread_playing = playing.clone();
        let thread_stop = stop.clone();
        let thread_volume = volume_bits.clone();
        let thread = thread::Builder::new()
            .name("kobo-bluealsa-output".into())
            .spawn(move || {
                let mut source = WorkerSource::new(audio_rx, state.clone(), channels, NonZeroU32::new(sample_rate_value).expect("sample rate is non-zero"), duration);
                let mut bytes = Vec::with_capacity(4096);
                let mut prebuffered = false;
                let mut first_write = true;
                while !thread_stop.load(Ordering::Acquire) {
                    if !thread_playing.load(Ordering::Acquire) {
                        thread::sleep(Duration::from_millis(20));
                        continue;
                    }
                    if !prebuffered {
                        let _prebuffer_timing = crate::OpeningTiming::new("prebuffer");
                        // Do not start feeding ALSA until the decoder has built a
                        // small cushion. Without this, WorkerSource emits silence
                        // while the first compressed blocks are being decoded.
                        let deadline = Instant::now() + KOBO_PREBUFFER_TIMEOUT;
                        while source.audio_rx.len() < KOBO_PREBUFFER_BLOCKS && !thread_stop.load(Ordering::Acquire) && Instant::now() < deadline {
                            thread::sleep(Duration::from_millis(10));
                        }
                        prebuffered = true;
                        eprintln!("AUDIOBOOK_TIMING phase=prebuffer queued_blocks={} target_blocks={KOBO_PREBUFFER_BLOCKS} timed_out={}", source.audio_rx.len(), Instant::now() >= deadline);
                    }
                    let Some(sample) = source.next() else { break };
                    let volume = f32::from_bits(thread_volume.load(Ordering::Acquire) as u32).clamp(0.0, 1.0);
                    let sample = (sample * volume).clamp(-1.0, 1.0);
                    bytes.extend_from_slice(&((sample * i16::MAX as f32) as i16).to_le_bytes());
                    if bytes.len() >= 4096 {
                        let first_write_timing = first_write.then(|| crate::OpeningTiming::new("first_pcm_write"));
                        if input.write_all(&bytes).is_err() {
                            if !thread_stop.load(Ordering::Acquire) {
                                state.output_lost.store(true, Ordering::Release);
                                if let Some(control) = &state.stream_control {
                                    control.set_paused(true);
                                }
                            }
                            thread_playing.store(false, Ordering::Release);
                            break;
                        }
                        drop(first_write_timing);
                        first_write = false;
                        bytes.clear();
                    }
                    if state.playback_finished() {
                        break;
                    }
                }
                if !bytes.is_empty() {
                    let _ = input.write_all(&bytes);
                }
                drop(input);
                thread_playing.store(false, Ordering::Release);
                // Never hold the child lock across a blocking wait: Drop must be
                // able to kill a helper stuck in ALSA (including after stdin EOF).
                let status = loop {
                    match worker_child.lock().expect("audio child lock poisoned").try_wait() {
                        Ok(Some(status)) => break Some(status),
                        Err(_) => break None,
                        Ok(None) => thread::sleep(Duration::from_millis(10)),
                    }
                };
                let stderr = stderr.and_then(|reader| reader.join().ok()).map(|bytes| String::from_utf8_lossy(&bytes).trim().to_owned()).unwrap_or_default();
                if !thread_stop.load(Ordering::Acquire) && status.is_some_and(|status| !status.success()) {
                    state.output_lost.store(true, Ordering::Release);
                    if let Some(control) = &state.stream_control {
                        control.set_paused(true);
                    }
                    let detail = if stderr.is_empty() { "aplay exited without a diagnostic".to_owned() } else { stderr };
                    log::warn!("Kobo audio output stopped: {detail}");
                    if let Some(message) = kobo_output_message(&detail) {
                        *state.error.lock().expect("audio error lock poisoned") = Some(message.to_owned());
                    }
                }
            })
            .map_err(|error| format!("Could not start Kobo audio output thread: {error}"))?;
        Ok(Self { playing, stop, volume_bits, child, thread: Some(thread) })
    }

    fn play(&self) {
        self.playing.store(true, Ordering::Release);
    }
    fn toggle(&self) {
        self.playing.fetch_xor(true, Ordering::AcqRel);
    }
    fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Acquire)
    }
    fn set_volume(&self, volume: f32) {
        self.volume_bits.store(volume.clamp(0.0, 1.0).to_bits() as u64, Ordering::Release);
    }
}

#[cfg(feature = "kobo")]
fn kobo_output_message(detail: &str) -> Option<&'static str> {
    let detail = detail.to_ascii_lowercase();
    if detail.contains("no such device") || detail.contains("disconnected") || detail.contains("broken pipe") { None } else { Some("Audio output stopped. Check your headphone connection, then press Play to retry.") }
}

#[cfg(feature = "kobo")]
fn connected_audio_device(cancelled: &impl Fn() -> bool) -> Result<String, String> {
    // One budget for the complete discovery, not one budget per paired device.
    let deadline = Instant::now() + Duration::from_secs(8);
    let output = bluetooth_output(std::process::Command::new("bluetoothctl").args(["devices"]), deadline, cancelled)?;
    for line in String::from_utf8_lossy(&output).lines() {
        let mut fields = line.split_whitespace();
        if fields.next() != Some("Device") {
            continue;
        }
        let Some(address) = fields.next() else { continue };
        if !is_bluetooth_address(address) {
            continue;
        }
        let info = match bluetooth_output(std::process::Command::new("bluetoothctl").args(["info", address]), deadline, cancelled) {
            Ok(info) => info,
            Err(error) if cancelled() || Instant::now() >= deadline => return Err(error),
            Err(_) => continue,
        };
        let info = String::from_utf8_lossy(&info);
        let connected = info.lines().any(|line| line.trim() == "Connected: yes");
        let audio_sink = info.lines().any(|line| line.contains("Audio Sink"));
        if connected && audio_sink {
            return Ok(address.to_owned());
        }
    }
    Err("No connected Bluetooth audio device found".to_owned())
}

#[cfg(feature = "kobo")]
fn bluetooth_output(command: &mut std::process::Command, deadline: Instant, cancelled: &impl Fn() -> bool) -> Result<Vec<u8>, String> {
    let interrupted = || {
        if cancelled() {
            Some("Audio output recovery cancelled")
        } else if Instant::now() >= deadline {
            Some("Bluetooth audio discovery timed out")
        } else {
            None
        }
    };
    if let Some(error) = interrupted() {
        return Err(error.into());
    }
    let mut child = command.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).spawn().map_err(|error| format!("Could not query Bluetooth audio: {error}"))?;
    let stdout = child.stdout.take().expect("Bluetooth command has piped stdout");
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        // Drain while the child runs so a large paired-device list cannot fill
        // the pipe and deadlock discovery. Bound retained command output.
        stdout.take(65_537).read_to_end(&mut bytes).map(|_| bytes)
    });
    let status = loop {
        if let Some(error) = interrupted() {
            break Err(error.to_owned());
        }
        match child.try_wait() {
            Ok(Some(status)) => break if status.success() { Ok(()) } else { Err("Bluetooth audio query failed".to_owned()) },
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(error) => break Err(format!("Could not wait for Bluetooth audio query: {error}")),
        }
    };
    if status.is_err() {
        let _ = child.kill();
    }
    let _ = child.wait();
    let bytes = reader.join().map_err(|_| "Bluetooth audio query reader stopped".to_owned())?.map_err(|error| format!("Could not read Bluetooth audio query: {error}"))?;
    status?;
    if bytes.len() > 65_536 {
        return Err("Bluetooth audio query output exceeded limit".into());
    }
    Ok(bytes)
}

#[cfg(feature = "kobo")]
fn is_bluetooth_address(value: &str) -> bool {
    value.len() == 17 && value.split(':').count() == 6 && value.split(':').all(|part| part.len() == 2 && part.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

#[cfg(feature = "kobo")]
impl Drop for KoboOutput {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.playing.store(true, Ordering::Release);
        // Interrupt a blocked pipe write before joining the output thread.
        // Checking/reaping under the same lock avoids targeting a reused PID.
        {
            let mut child = self.child.lock().expect("audio child lock poisoned");
            if matches!(child.try_wait(), Ok(None)) {
                let _ = child.kill();
            }
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "kobo")]
    #[test]
    fn headphone_disconnect_is_silent_but_other_output_failures_are_reported() {
        assert_eq!(kobo_output_message("pcm_write:2050: write error: No such device"), None);
        assert_eq!(kobo_output_message("Broken pipe"), None);
        assert_eq!(kobo_output_message("Disconnected"), None);
        assert!(kobo_output_message("unsupported sample format").is_some());
    }

    #[cfg(all(feature = "kobo", unix))]
    #[test]
    fn kobo_pipe_loss_pauses_silently_and_retry_reseeks_before_output() {
        fn child(script: &str) -> std::process::Child {
            std::process::Command::new("/bin/sh").args(["-c", script]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped()).spawn().unwrap()
        }
        let mut state = test_state(0, 42_000_000);
        let control = app::AudioStreamControl::default();
        Arc::get_mut(&mut state).unwrap().stream_control = Some(control.clone());
        let (samples, receiver) = bounded(BUFFERED_BLOCKS);
        for _ in 0..8 {
            samples.send(AudioChunk { generation: 0, tempo: 1.0, samples: vec![0.0; BLOCK_SAMPLES] }).unwrap();
        }
        let channels = NonZeroU16::new(1).unwrap();
        let rate = NonZeroU32::new(22050).unwrap();
        let duration = Duration::from_secs(100);
        let failed_child = child("dd bs=4096 count=1 >/dev/null 2>&1; echo 'pcm_write:2050: write error: No such device' >&2; exit 1");
        let output = KoboOutput::from_child(failed_child, receiver.clone(), state.clone(), channels, rate, duration).unwrap();
        output.play();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !output.thread.as_ref().unwrap().is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(output.thread.as_ref().unwrap().is_finished(), "failed pipe did not stop its output thread");
        assert!(!output.is_playing());
        assert!(state.output_lost.load(Ordering::Acquire));
        assert!(control.is_paused());
        assert!(state.error.lock().unwrap().is_none(), "headphone loss must not show a decoding error");
        let (request_tx, requests) = unbounded();
        let mut engine = AudioEngine { recovery: Mutex::new(None), stream_control: Some(control.clone()), output, retry_output: (receiver.clone(), channels, rate), request_tx, worker: None, state: state.clone(), duration };
        let confirmed = engine.last_confirmed_position();
        let replacement = KoboOutput::from_child(child("exec cat >/dev/null"), receiver, state.clone(), channels, rate, duration).unwrap();
        assert!(!replacement.is_playing(), "a reconnected output must not start by itself");
        assert!(!engine.is_playing());
        assert!(control.is_paused());
        engine.restart_output(replacement).unwrap();
        assert!(matches!(requests.recv().unwrap(), DecoderRequest::Seek { position, generation: 1 } if position == confirmed));
        assert_eq!(state.generation.load(Ordering::Acquire), 1, "retry must invalidate the old PCM queue");
        assert!(engine.is_playing());
        assert!(!control.is_paused());
        assert!(!engine.take_output_lost());
        drop(engine);
        drop(samples);
    }

    #[cfg(all(feature = "kobo", unix))]
    #[test]
    fn recovery_worker_returns_control_before_discovery_completes() {
        let (started, running) = bounded(1);
        let (release, released) = bounded(1);
        let (finished, done) = bounded(1);
        let recovery = OutputRecovery::start(move |cancelled| {
            started.send(()).unwrap();
            released.recv_timeout(Duration::from_secs(2)).unwrap();
            finished.send(cancelled.load(Ordering::Acquire)).unwrap();
            Err("cancelled fixture discovery".into())
        })
        .unwrap();
        running.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(recovery.result.try_recv(), Err(TryRecvError::Empty)));
        drop(recovery);
        release.send(()).unwrap();
        assert!(done.recv_timeout(Duration::from_secs(2)).unwrap());
    }

    #[cfg(all(feature = "kobo", unix))]
    #[test]
    fn bluetooth_query_is_bounded_and_cancellable() {
        let started = Instant::now();
        let result = bluetooth_output(std::process::Command::new("/bin/sh").args(["-c", "exec sleep 30"]), started + Duration::from_millis(100), &|| false);
        assert!(result.unwrap_err().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(2));

        let cancelled = Arc::new(AtomicBool::new(false));
        let signal = cancelled.clone();
        let cancel = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            signal.store(true, Ordering::Release);
        });
        let started = Instant::now();
        let result = bluetooth_output(std::process::Command::new("/bin/sh").args(["-c", "exec sleep 30"]), started + Duration::from_secs(8), &|| cancelled.load(Ordering::Acquire));
        cancel.join().unwrap();
        assert!(result.unwrap_err().contains("cancelled"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[cfg(all(feature = "kobo", unix))]
    #[test]
    fn bluetooth_query_drains_output_and_honors_shared_deadline_before_spawn() {
        let result = bluetooth_output(std::process::Command::new("/bin/sh").args(["-c", "head -c 60000 /dev/zero"]), Instant::now() + Duration::from_secs(2), &|| false).unwrap();
        assert_eq!(result.len(), 60_000);
        let result = bluetooth_output(&mut std::process::Command::new("/this-command-must-not-be-spawned"), Instant::now(), &|| false);
        assert!(result.unwrap_err().contains("timed out"));
    }

    #[cfg(all(feature = "kobo", unix))]
    #[test]
    fn recovery_pause_discards_late_output_and_new_recovery_preserves_seek() {
        let state = test_state(0, 42_000_000);
        let (_samples, receiver) = bounded(BUFFERED_BLOCKS);
        let channels = NonZeroU16::new(1).unwrap();
        let rate = NonZeroU32::new(22050).unwrap();
        let duration = Duration::from_secs(100);
        let output = |receiver: Receiver<AudioChunk>, state: Arc<SharedState>| {
            let child = std::process::Command::new("/bin/sh").args(["-c", "exec cat >/dev/null"]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped()).spawn().unwrap();
            KoboOutput::from_child(child, receiver, state, channels, rate, duration).unwrap()
        };
        let (request_tx, requests) = unbounded();
        let mut engine =
            AudioEngine { recovery: Mutex::new(None), stream_control: None, output: output(receiver.clone(), state.clone()), retry_output: (receiver.clone(), channels, rate), request_tx, worker: None, state: state.clone(), duration };
        let (deliver, result) = bounded(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        *engine.recovery.lock().unwrap() = Some(OutputRecovery { cancelled: cancelled.clone(), result });
        assert!(engine.is_buffering());
        assert!(!engine.is_playing());
        engine.pause();
        assert!(cancelled.load(Ordering::Acquire));
        assert!(!engine.is_buffering());
        assert!(deliver.send(Ok(output(receiver.clone(), state.clone()))).is_err());
        engine.refresh_output();
        assert!(!engine.is_playing());
        assert!(requests.try_recv().is_err());

        let (deliver, result) = bounded(1);
        *engine.recovery.lock().unwrap() = Some(OutputRecovery { cancelled: Arc::new(AtomicBool::new(false)), result });
        engine.seek_to(Duration::from_secs(70)).unwrap();
        assert!(matches!(requests.recv().unwrap(), DecoderRequest::Seek { generation: 1, .. }));
        deliver.send(Ok(output(receiver, state.clone()))).unwrap_or_else(|_| panic!("recovery receiver closed"));
        engine.refresh_output();
        assert!(matches!(requests.recv().unwrap(), DecoderRequest::Seek { position, generation: 2 } if position == Duration::from_secs(70)));
        assert!(engine.is_playing());
        let (_, result) = bounded(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        *engine.recovery.lock().unwrap() = Some(OutputRecovery { cancelled: cancelled.clone(), result });
        drop(engine);
        assert!(cancelled.load(Ordering::Acquire), "Close must cancel discovery before releasing the engine");
    }

    #[cfg(all(feature = "kobo", unix))]
    #[test]
    fn kobo_close_interrupts_a_helper_that_never_reads_pcm() {
        let state = test_state(0, 0);
        let (samples, receiver) = bounded(BUFFERED_BLOCKS);
        for _ in 0..BUFFERED_BLOCKS {
            samples.send(AudioChunk { generation: 0, tempo: 1.0, samples: vec![0.0; BLOCK_SAMPLES] }).unwrap();
        }
        let child = std::process::Command::new("/bin/sh").args(["-c", "exec sleep 30"]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped()).spawn().unwrap();
        let output = KoboOutput::from_child(child, receiver, state.clone(), NonZeroU16::new(1).unwrap(), NonZeroU32::new(22050).unwrap(), Duration::from_secs(100)).unwrap();
        let cleanup = output.child.clone();
        output.play();
        let deadline = Instant::now() + Duration::from_secs(2);
        while state.position_micros() == 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        let consumed = state.position_micros();
        let (done, finished) = std::sync::mpsc::channel();
        let closing = thread::spawn(move || {
            drop(output);
            let _ = done.send(());
        });
        let closed = finished.recv_timeout(Duration::from_secs(2)).is_ok();
        // A regression must fail promptly rather than leave a helper behind or
        // hang the entire suite in the output thread's blocking pipe write.
        if !closed {
            let _ = cleanup.lock().unwrap().kill();
        }
        closing.join().unwrap();
        assert!(consumed > 0, "output never began consuming PCM");
        assert!(closed, "Close waited for an unresponsive audio helper");
        assert!(state.error.lock().unwrap().is_none());
        assert!(!state.output_lost.load(Ordering::Acquire));
    }

    #[cfg(all(feature = "kobo", unix))]
    #[test]
    fn kobo_completed_output_restarts_at_zero_even_before_helper_exits() {
        let state = test_state(0, 100_000_000);
        state.playback_finished_generation.store(0, Ordering::Release);
        let (_samples, receiver) = bounded(BUFFERED_BLOCKS);
        let channels = NonZeroU16::new(1).unwrap();
        let rate = NonZeroU32::new(22050).unwrap();
        let duration = Duration::from_secs(100);
        let output = || {
            let child = std::process::Command::new("/bin/sh").args(["-c", "exec cat >/dev/null"]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped()).spawn().unwrap();
            KoboOutput::from_child(child, receiver.clone(), state.clone(), channels, rate, duration).unwrap()
        };
        let (request_tx, requests) = unbounded();
        let mut engine = AudioEngine { recovery: Mutex::new(None), stream_control: None, output: output(), retry_output: (receiver.clone(), channels, rate), request_tx, worker: None, state: state.clone(), duration };
        assert!(!engine.output.thread.as_ref().unwrap().is_finished());
        engine.restart_output(output()).unwrap();
        assert!(matches!(requests.recv().unwrap(), DecoderRequest::Seek { position: Duration::ZERO, generation: 1 }));
        assert!(!state.playback_finished());
        assert!(engine.is_playing());
    }

    fn test_state(generation: u64, position_us: u64) -> Arc<SharedState> {
        Arc::new(SharedState {
            buffering: AtomicBool::new(false),
            output_lost: AtomicBool::new(false),
            stream_control: None,
            decoder_stopped: AtomicBool::new(false),
            resume_blocks: 1,
            generation: AtomicU64::new(generation),
            position: Mutex::new(PositionState { generation, micros: position_us }),
            confirmed_position: Mutex::new(PositionState { generation, micros: position_us }),
            decoder_end_generation: AtomicU64::new(NO_GENERATION),
            playback_finished_generation: AtomicU64::new(NO_GENERATION),
            error: Mutex::new(None),
        })
    }

    fn test_source(audio_rx: Receiver<AudioChunk>, state: Arc<SharedState>, channels: u16, sample_rate: u32) -> WorkerSource {
        WorkerSource::new(audio_rx, state, NonZeroU16::new(channels).unwrap(), NonZeroU32::new(sample_rate).unwrap(), Duration::from_secs(100))
    }

    /// Exercises the actual MP4 demuxer and MP3 decoder without an audio device.
    /// Run explicitly with `cargo test --release -p audiobook-player --lib
    /// mp3_in_m4b_seek_interrupts_blocked_input -- --ignored`.
    #[test]
    #[ignore = "requires ffmpeg with libmp3lame"]
    fn mp3_in_m4b_seek_interrupts_blocked_input() {
        use std::io::{Cursor, Read, Seek};
        struct StallingReader {
            data: Cursor<Vec<u8>>,
            stall: Arc<AtomicBool>,
            control: app::AudioStreamControl,
            started: std::sync::mpsc::Sender<()>,
        }
        impl Read for StallingReader {
            fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
                if self.control.is_cancelled() {
                    return Err(std::io::ErrorKind::ConnectionAborted.into());
                }
                if self.control.is_interrupted() {
                    return Err(std::io::ErrorKind::WouldBlock.into());
                }
                if self.stall.load(Ordering::Acquire) {
                    let _ = self.started.send(());
                    loop {
                        if self.control.is_cancelled() {
                            return Err(std::io::ErrorKind::ConnectionAborted.into());
                        }
                        if self.control.is_interrupted() {
                            self.stall.store(false, Ordering::Release);
                            return Err(std::io::ErrorKind::WouldBlock.into());
                        }
                        thread::sleep(Duration::from_millis(2));
                    }
                }
                self.data.read(output)
            }
        }
        impl Seek for StallingReader {
            fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
                self.data.seek(position)
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("mp3-in-m4b.m4b");
        let output = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=44100:duration=30", "-c:a", "libmp3lame", "-b:a", "128k", "-movflags", "+faststart", "-f", "mp4"])
            .arg(&path)
            .output()
            .expect("this integration test requires ffmpeg");
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let control = app::AudioStreamControl::default();
        let stall = Arc::new(AtomicBool::new(false));
        let (started, waiting) = std::sync::mpsc::channel();
        let decoder = open_decoder(Box::new(StallingReader { data: Cursor::new(std::fs::read(&path).unwrap()), stall: stall.clone(), control: control.clone(), started })).unwrap();
        let channels = decoder.channels();
        let sample_rate = decoder.sample_rate();
        let mut state = test_state(0, 0);
        Arc::get_mut(&mut state).unwrap().stream_control = Some(control.clone());
        let (request_tx, request_rx) = unbounded();
        let (audio_tx, audio_rx) = bounded(128);
        let worker_state = state.clone();
        let worker_control = control.clone();
        stall.store(true, Ordering::Release);
        let worker = thread::spawn(move || decode_worker(decoder, channels, sample_rate, request_rx, audio_tx, worker_state, Some(worker_control)));
        let blocked = waiting.recv_timeout(Duration::from_secs(3));
        if blocked.is_err() {
            control.cancel();
            let _ = request_tx.send(DecoderRequest::Stop);
            let _ = worker.join();
            panic!("decoder did not reach the injected input stall");
        }
        let seek_started = Instant::now();
        request_seek(&state, &request_tx, Duration::from_secs(15)).unwrap();
        let mut resumed = None;
        while seek_started.elapsed() < Duration::from_secs(3) {
            match audio_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(chunk) if chunk.generation == 1 && !chunk.samples.is_empty() => {
                    resumed = Some(chunk.samples);
                    break;
                }
                Ok(_) | Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            }
        }
        control.cancel();
        request_tx.send(DecoderRequest::Stop).unwrap();
        assert!(worker.join().unwrap().is_ok());
        let resumed = resumed.expect("MP3-in-M4B decoder did not resume after interrupted input");
        assert_eq!(state.confirmed_position_micros(), Some(15_000_000));
        assert!(state.error.lock().unwrap().is_none());
        let mut reference = open_decoder(Box::new(Cursor::new(std::fs::read(path).unwrap()))).unwrap();
        reference.try_seek(Duration::from_secs(15)).unwrap();
        let expected = read_decoder_block(&mut reference, BLOCK_SAMPLES / channels.get() as usize * channels.get() as usize);
        assert_eq!(resumed, expected, "recovered PCM must match an uninterrupted seek to the same position");
    }

    #[test]
    fn streaming_buffering_holds_position_until_prebuffer_is_ready() {
        let (audio_tx, audio_rx) = bounded(8);
        let mut state = test_state(0, 42_000_000);
        Arc::get_mut(&mut state).unwrap().resume_blocks = 6;
        let mut source = test_source(audio_rx, state.clone(), 1, 256);
        assert_eq!(source.next(), Some(0.0));
        assert!(state.buffering.load(Ordering::Acquire));
        for _ in 0..5 {
            audio_tx.send(AudioChunk { generation: 0, tempo: 1.0, samples: vec![0.5; 256] }).unwrap();
            assert_eq!(source.next(), Some(0.0));
            assert_eq!(state.position_micros(), 42_000_000);
        }
        audio_tx.send(AudioChunk { generation: 0, tempo: 1.0, samples: vec![0.5; 256] }).unwrap();
        assert_eq!(source.next(), Some(0.5));
        assert!(!state.buffering.load(Ordering::Acquire));
    }

    #[test]
    fn terminal_stream_failure_drains_short_tail_without_marking_eof() {
        let (audio_tx, audio_rx) = bounded(8);
        let mut state = test_state(0, 42_000_000);
        Arc::get_mut(&mut state).unwrap().resume_blocks = 6;
        let mut source = test_source(audio_rx, state.clone(), 1, 256);
        assert_eq!(source.next(), Some(0.0));
        audio_tx.send(AudioChunk { generation: 0, tempo: 1.0, samples: vec![0.5; 256] }).unwrap();
        drop(audio_tx);
        state.decoder_stopped.store(true, Ordering::Release);
        for _ in 0..256 {
            assert_eq!(source.next(), Some(0.5));
        }
        assert_eq!(source.next(), None);
        assert_eq!(state.position_micros(), 43_000_000);
        assert!(!state.playback_finished());
    }

    #[test]
    fn seek_discards_the_current_old_generation_before_updating_position() {
        let (_audio_tx, audio_rx) = bounded(1);
        let state = test_state(1, 42_000_000);
        let mut source = test_source(audio_rx, state.clone(), 1, 1);
        source.current = vec![0.75];
        source.current_generation = Some(0);
        source.source_position_us = 5_000_000.0;

        assert_eq!(source.next(), Some(0.0));
        assert_eq!(state.position_micros(), 42_000_000);
    }

    #[test]
    fn stale_queued_chunks_are_skipped_after_a_seek() {
        let (audio_tx, audio_rx) = bounded(2);
        audio_tx.send(AudioChunk { generation: 0, tempo: 1.0, samples: vec![0.25] }).unwrap();
        audio_tx.send(AudioChunk { generation: 1, tempo: 1.0, samples: vec![0.75] }).unwrap();
        let state = test_state(1, 10_000_000);
        let mut source = test_source(audio_rx, state, 1, 1);

        assert_eq!(source.next(), Some(0.75));
    }

    #[test]
    fn position_accumulates_across_chunk_boundaries() {
        let (audio_tx, audio_rx) = bounded(2);
        for _ in 0..2 {
            audio_tx.send(AudioChunk { generation: 0, tempo: 1.0, samples: vec![0.5; 128] }).unwrap();
        }
        let state = test_state(0, 0);
        let mut source = test_source(audio_rx, state.clone(), 1, 256);

        for _ in 0..256 {
            assert_eq!(source.next(), Some(0.5));
        }
        assert_eq!(state.position_micros(), 1_000_000);
    }

    #[test]
    fn position_uses_source_time_at_non_default_tempo() {
        let (audio_tx, audio_rx) = bounded(1);
        audio_tx.send(AudioChunk { generation: 0, tempo: 2.0, samples: vec![0.5; 256] }).unwrap();
        let state = test_state(0, 3_000_000);
        let mut source = test_source(audio_rx, state.clone(), 1, 256);

        for _ in 0..256 {
            source.next();
        }
        assert_eq!(state.position_micros(), 5_000_000);
    }

    #[test]
    fn stereo_samples_advance_position_once_per_frame() {
        let (audio_tx, audio_rx) = bounded(1);
        audio_tx.send(AudioChunk { generation: 0, tempo: 1.0, samples: vec![0.5; 512] }).unwrap();
        let state = test_state(0, 0);
        let mut source = test_source(audio_rx, state.clone(), 2, 256);

        for _ in 0..512 {
            source.next();
        }
        assert_eq!(state.position_micros(), 1_000_000);
    }

    #[test]
    fn disconnected_source_ends_after_buffered_audio() {
        let (audio_tx, audio_rx) = bounded(1);
        audio_tx.send(AudioChunk { generation: 0, tempo: 1.0, samples: vec![0.5] }).unwrap();
        drop(audio_tx);
        let mut source = test_source(audio_rx, test_state(0, 0), 1, 1);

        assert_eq!(source.next(), Some(0.5));
        assert_eq!(source.next(), None);
    }

    #[test]
    fn decoder_end_keeps_source_alive_and_marks_playback_finished() {
        let (_audio_tx, audio_rx) = bounded(1);
        let state = test_state(0, 90_000_000);
        state.decoder_end_generation.store(0, Ordering::Release);
        let mut source = test_source(audio_rx, state.clone(), 1, 1);

        assert_eq!(source.next(), Some(0.0));
        assert_eq!(state.position_micros(), 100_000_000);
        assert!(state.playback_finished());
    }

    #[test]
    fn stale_end_of_stream_cannot_overwrite_a_newer_chapter_seek() {
        let (_audio_tx, audio_rx) = bounded(1);
        let state = test_state(2, 25_000_000);
        state.decoder_end_generation.store(1, Ordering::Release);
        let mut source = test_source(audio_rx, state.clone(), 1, 1);

        assert_eq!(source.next(), Some(0.0));
        assert_eq!(state.position_micros(), 25_000_000);
        assert!(!state.playback_finished());
    }

    #[test]
    fn failed_seek_submission_rolls_back_generation_and_position() {
        let (request_tx, request_rx) = unbounded();
        drop(request_rx);
        let state = test_state(7, 12_000_000);

        assert_eq!(request_seek(&state, &request_tx, Duration::from_secs(80)), Err("audio decoder stopped".to_owned()));
        assert_eq!(state.generation.load(Ordering::Acquire), 7);
        assert_eq!(state.position_micros(), 12_000_000);
    }

    #[test]
    fn decoder_seek_failure_restores_the_last_confirmed_position() {
        let (request_tx, _request_rx) = unbounded();
        let state = test_state(7, 12_000_000);
        request_seek(&state, &request_tx, Duration::from_secs(80)).unwrap();

        assert!(state.reject_seek_if_current(8, "seek failed".to_owned()));

        assert_eq!(state.generation.load(Ordering::Acquire), 7);
        assert_eq!(state.position_micros(), 12_000_000);
        assert_eq!(state.confirmed_position_micros(), Some(12_000_000));
        assert_eq!(state.error.lock().unwrap().as_deref(), Some("seek failed"));
    }

    #[test]
    fn stale_decoder_seek_failure_cannot_roll_back_a_newer_request() {
        let (request_tx, _request_rx) = unbounded();
        let state = test_state(0, 0);
        request_seek(&state, &request_tx, Duration::from_secs(10)).unwrap();
        request_seek(&state, &request_tx, Duration::from_secs(20)).unwrap();

        assert!(!state.reject_seek_if_current(1, "stale failure".to_owned()));

        assert_eq!(state.generation.load(Ordering::Acquire), 2);
        assert_eq!(state.position_micros(), 20_000_000);
        assert_eq!(state.confirmed_position_micros(), None);
        assert!(state.error.lock().unwrap().is_none());
    }

    #[test]
    fn pending_seek_has_no_persistable_position_until_the_decoder_accepts_it() {
        let (request_tx, _request_rx) = unbounded();
        let state = test_state(0, 12_000_000);
        request_seek(&state, &request_tx, Duration::from_secs(80)).unwrap();

        assert_eq!(state.confirmed_position_micros(), None);

        state.confirm_seek(1, Duration::from_secs(80));
        assert_eq!(state.confirmed_position_micros(), Some(80_000_000));
    }

    #[test]
    fn confirmed_seek_becomes_the_rollback_point_for_the_next_request() {
        let (request_tx, _request_rx) = unbounded();
        let state = test_state(0, 0);
        request_seek(&state, &request_tx, Duration::from_secs(10)).unwrap();
        state.confirm_seek(1, Duration::from_secs(10));
        request_seek(&state, &request_tx, Duration::from_secs(20)).unwrap();

        assert!(state.reject_seek_if_current(2, "seek failed".to_owned()));

        assert_eq!(state.generation.load(Ordering::Acquire), 1);
        assert_eq!(state.position_micros(), 10_000_000);
        assert_eq!(state.confirmed_position_micros(), Some(10_000_000));
    }

    #[test]
    fn rapid_seek_burst_preserves_the_latest_request_without_backpressure() {
        let (request_tx, request_rx) = unbounded();
        let state = test_state(0, 0);

        for second in 1..=10_000 {
            request_seek(&state, &request_tx, Duration::from_secs(second)).unwrap();
        }

        assert_eq!(request_rx.len(), 10_000);
        assert_eq!(state.generation.load(Ordering::Acquire), 10_000);
        assert_eq!(state.position_micros(), 10_000_000_000);
        let pending = collect_pending_requests(None, &request_rx);
        assert_eq!(pending.seek, Some((Duration::from_secs(10_000), 10_000)));
        assert_eq!(request_rx.len(), 0);
    }

    #[test]
    fn stale_position_updates_never_win_during_long_seek_sequence() {
        let state = test_state(0, 0);

        for generation in 1..=1_000 {
            state.generation.store(generation, Ordering::Release);
            *state.position.lock().unwrap() = PositionState { generation, micros: generation * 1_000_000 };
            assert!(!state.set_position_if_generation(generation - 1, u64::MAX));
            assert_eq!(state.position_micros(), generation * 1_000_000);
        }
    }

    #[test]
    fn seeking_after_finish_immediately_clears_finished_state() {
        let (request_tx, _request_rx) = unbounded();
        let state = test_state(3, 100_000_000);
        state.playback_finished_generation.store(3, Ordering::Release);
        assert!(state.playback_finished());

        request_seek(&state, &request_tx, Duration::from_secs(20)).unwrap();

        assert!(!state.playback_finished());
        assert_eq!(state.position_micros(), 20_000_000);
    }

    #[test]
    fn tempo_rejects_non_finite_values_and_clamps_extremes() {
        assert!(normalized_tempo(f32::NAN).is_err());
        assert!(normalized_tempo(f32::INFINITY).is_err());
        assert_eq!(normalized_tempo(0.0).unwrap(), wsola::MIN_TEMPO);
        assert_eq!(normalized_tempo(100.0).unwrap(), wsola::MAX_TEMPO);
        assert_eq!(normalized_tempo(1.25).unwrap(), 1.25);
    }

    #[test]
    fn normal_tempo_uses_direct_decode_path() {
        assert!(is_realtime_tempo(1.0));
        assert!(is_realtime_tempo(1.00005));
        assert!(!is_realtime_tempo(0.99));
        assert!(!is_realtime_tempo(1.01));
    }

    #[test]
    fn seek_generation_exhaustion_fails_without_mutating_state() {
        let (request_tx, _request_rx) = unbounded();
        let state = test_state(NO_GENERATION - 1, 17_000_000);

        assert_eq!(request_seek(&state, &request_tx, Duration::from_secs(80)), Err("audio seek generation exhausted".to_owned()));
        assert_eq!(state.generation.load(Ordering::Acquire), NO_GENERATION - 1);
        assert_eq!(state.position_micros(), 17_000_000);
    }

    #[test]
    fn decoded_position_never_advances_past_total_duration() {
        let (audio_tx, audio_rx) = bounded(1);
        audio_tx.send(AudioChunk { generation: 0, tempo: 4.0, samples: vec![0.5; 256] }).unwrap();
        let state = test_state(0, 99_000_000);
        let mut source = test_source(audio_rx, state.clone(), 1, 256);

        for _ in 0..256 {
            source.next();
        }
        assert_eq!(state.position_micros(), 100_000_000);
    }

    #[test]
    fn collecting_no_requests_produces_no_work() {
        let (_request_tx, request_rx) = unbounded();

        let pending = collect_pending_requests(None, &request_rx);

        assert_eq!(pending.seek, None);
        assert_eq!(pending.tempo, None);
        assert!(!pending.stop);
    }

    #[test]
    fn mixed_request_burst_coalesces_seek_and_tempo_independently() {
        let (request_tx, request_rx) = unbounded();
        request_tx.send(DecoderRequest::Seek { position: Duration::from_secs(4), generation: 1 }).unwrap();
        request_tx.send(DecoderRequest::Tempo(0.75)).unwrap();
        request_tx.send(DecoderRequest::Seek { position: Duration::from_secs(9), generation: 2 }).unwrap();
        request_tx.send(DecoderRequest::Tempo(1.5)).unwrap();

        let pending = collect_pending_requests(None, &request_rx);

        assert_eq!(pending.seek, Some((Duration::from_secs(9), 2)));
        assert_eq!(pending.tempo, Some(1.5));
        assert!(!pending.stop);
    }

    #[test]
    fn first_request_and_queued_requests_are_coalesced_together() {
        let (request_tx, request_rx) = unbounded();
        request_tx.send(DecoderRequest::Seek { position: Duration::from_secs(8), generation: 4 }).unwrap();
        request_tx.send(DecoderRequest::Tempo(2.0)).unwrap();

        let pending = collect_pending_requests(Some(DecoderRequest::Tempo(0.5)), &request_rx);

        assert_eq!(pending.seek, Some((Duration::from_secs(8), 4)));
        assert_eq!(pending.tempo, Some(2.0));
    }

    #[test]
    fn stop_is_preserved_while_the_queue_is_drained() {
        let (request_tx, request_rx) = unbounded();
        request_tx.send(DecoderRequest::Stop).unwrap();
        request_tx.send(DecoderRequest::Seek { position: Duration::from_secs(12), generation: 7 }).unwrap();

        let pending = collect_pending_requests(None, &request_rx);

        assert!(pending.stop);
        assert_eq!(pending.seek, Some((Duration::from_secs(12), 7)));
        assert!(request_rx.is_empty());
    }

    #[test]
    fn successful_seek_updates_state_and_enqueues_the_same_generation() {
        let (request_tx, request_rx) = unbounded();
        let state = test_state(41, 3_000_000);
        let target = Duration::from_micros(7_654_321);

        request_seek(&state, &request_tx, target).unwrap();

        assert_eq!(state.generation.load(Ordering::Acquire), 42);
        assert_eq!(state.position_micros(), 7_654_321);
        match request_rx.recv().unwrap() {
            DecoderRequest::Seek { position, generation } => {
                assert_eq!(position, target);
                assert_eq!(generation, 42);
            }
            _ => panic!("expected seek request"),
        }
    }

    #[test]
    fn failed_seek_restores_a_preexisting_finished_state() {
        let (request_tx, request_rx) = unbounded();
        drop(request_rx);
        let state = test_state(5, 100_000_000);
        state.playback_finished_generation.store(5, Ordering::Release);

        assert!(request_seek(&state, &request_tx, Duration::from_secs(10)).is_err());

        assert_eq!(state.generation.load(Ordering::Acquire), 5);
        assert_eq!(state.position_micros(), 100_000_000);
        assert!(state.playback_finished());
    }

    #[test]
    fn position_update_only_accepts_the_current_position_generation() {
        let state = test_state(3, 1_000_000);

        assert!(!state.set_position_if_generation(2, 99_000_000));
        assert_eq!(state.position_micros(), 1_000_000);
        assert!(state.set_position_if_generation(3, 2_000_000));
        assert_eq!(state.position_micros(), 2_000_000);
    }

    #[test]
    fn finished_flag_only_applies_to_the_current_generation() {
        let state = test_state(9, 0);

        state.playback_finished_generation.store(8, Ordering::Release);
        assert!(!state.playback_finished());
        state.playback_finished_generation.store(9, Ordering::Release);
        assert!(state.playback_finished());
        state.generation.store(10, Ordering::Release);
        assert!(!state.playback_finished());
    }

    #[test]
    fn an_empty_live_source_yields_silence_without_finishing() {
        let (_audio_tx, audio_rx) = bounded(1);
        let state = test_state(0, 15_000_000);
        let mut source = test_source(audio_rx, state.clone(), 1, 48_000);

        assert_eq!(source.next(), Some(0.0));
        assert_eq!(state.position_micros(), 15_000_000);
        assert!(!state.playback_finished());
    }

    #[test]
    fn eof_waits_until_the_buffered_chunk_has_been_consumed() {
        let (audio_tx, audio_rx) = bounded(1);
        audio_tx.send(AudioChunk { generation: 0, tempo: 1.0, samples: vec![0.25] }).unwrap();
        let state = test_state(0, 99_000_000);
        state.decoder_end_generation.store(0, Ordering::Release);
        let mut source = test_source(audio_rx, state.clone(), 1, 1);

        assert_eq!(source.next(), Some(0.25));
        assert!(!state.playback_finished());
        assert_eq!(source.next(), Some(0.0));
        assert!(state.playback_finished());
        assert_eq!(state.position_micros(), 100_000_000);
    }

    #[test]
    fn eof_on_a_zero_duration_source_finishes_at_zero() {
        let (_audio_tx, audio_rx) = bounded(1);
        let state = test_state(0, 0);
        state.decoder_end_generation.store(0, Ordering::Release);
        let mut source = WorkerSource::new(audio_rx, state.clone(), NonZeroU16::new(1).unwrap(), NonZeroU32::new(48_000).unwrap(), Duration::ZERO);

        assert_eq!(source.next(), Some(0.0));
        assert_eq!(state.position_micros(), 0);
        assert!(state.playback_finished());
    }

    #[test]
    fn surround_audio_advances_position_once_per_complete_frame() {
        let (audio_tx, audio_rx) = bounded(1);
        audio_tx.send(AudioChunk { generation: 0, tempo: 1.0, samples: vec![0.5; 256 * 6] }).unwrap();
        let state = test_state(0, 0);
        let mut source = test_source(audio_rx, state.clone(), 6, 256);

        for _ in 0..256 * 6 {
            source.next();
        }

        assert_eq!(state.position_micros(), 1_000_000);
    }

    #[test]
    fn position_is_published_at_the_exact_update_interval() {
        let (audio_tx, audio_rx) = bounded(2);
        audio_tx.send(AudioChunk { generation: 0, tempo: 1.0, samples: vec![0.5; 255] }).unwrap();
        audio_tx.send(AudioChunk { generation: 0, tempo: 1.0, samples: vec![0.5] }).unwrap();
        let state = test_state(0, 4_000_000);
        let mut source = test_source(audio_rx, state.clone(), 1, 256);

        for _ in 0..255 {
            source.next();
        }
        assert_eq!(state.position_micros(), 4_000_000);
        source.next();
        assert_eq!(state.position_micros(), 5_000_000);
    }

    #[test]
    fn generation_change_resets_partial_position_accounting() {
        let (audio_tx, audio_rx) = bounded(2);
        audio_tx.send(AudioChunk { generation: 0, tempo: 1.0, samples: vec![0.5; 255] }).unwrap();
        let state = test_state(0, 0);
        let mut source = test_source(audio_rx, state.clone(), 1, 256);
        for _ in 0..255 {
            source.next();
        }

        state.generation.store(1, Ordering::Release);
        *state.position.lock().unwrap() = PositionState { generation: 1, micros: 10_000_000 };
        audio_tx.send(AudioChunk { generation: 1, tempo: 1.0, samples: vec![0.75; 256] }).unwrap();
        for _ in 0..256 {
            assert_eq!(source.next(), Some(0.75));
        }

        assert_eq!(state.position_micros(), 11_000_000);
    }

    #[test]
    fn tempo_changes_between_chunks_accumulate_source_time_correctly() {
        let (audio_tx, audio_rx) = bounded(2);
        audio_tx.send(AudioChunk { generation: 0, tempo: 0.5, samples: vec![0.5; 128] }).unwrap();
        audio_tx.send(AudioChunk { generation: 0, tempo: 1.5, samples: vec![0.5; 128] }).unwrap();
        let state = test_state(0, 2_000_000);
        let mut source = test_source(audio_rx, state.clone(), 1, 256);

        for _ in 0..256 {
            source.next();
        }

        assert_eq!(state.position_micros(), 3_000_000);
    }

    #[test]
    fn stale_chunks_followed_by_disconnect_end_cleanly() {
        let (audio_tx, audio_rx) = bounded(2);
        audio_tx.send(AudioChunk { generation: 2, tempo: 1.0, samples: vec![0.25; 8] }).unwrap();
        drop(audio_tx);
        let state = test_state(3, 7_000_000);
        let mut source = test_source(audio_rx, state.clone(), 1, 1);

        assert_eq!(source.next(), None);
        assert_eq!(state.position_micros(), 7_000_000);
    }

    #[test]
    fn source_reports_its_audio_format_and_duration() {
        let (_audio_tx, audio_rx) = bounded(1);
        let source = WorkerSource::new(audio_rx, test_state(0, 0), NonZeroU16::new(2).unwrap(), NonZeroU32::new(44_100).unwrap(), Duration::from_secs(321));

        assert_eq!(source.channels(), NonZeroU16::new(2).unwrap());
        assert_eq!(source.sample_rate(), NonZeroU32::new(44_100).unwrap());
        assert_eq!(source.total_duration(), Some(Duration::from_secs(321)));
        assert_eq!(source.current_span_len(), None);
    }

    struct IntermittentDecoder {
        reads: std::vec::IntoIter<Option<f32>>,
    }

    impl Iterator for IntermittentDecoder {
        type Item = f32;

        fn next(&mut self) -> Option<Self::Item> {
            self.reads.next().flatten()
        }
    }

    #[test]
    fn transient_empty_decoder_reads_are_not_treated_as_eof() {
        for empty_reads in 1..EOF_CONFIRMATION_READS {
            let mut reads = vec![None; empty_reads];
            reads.extend([Some(0.25), Some(0.5), None]);
            let mut decoder = IntermittentDecoder { reads: reads.into_iter() };

            assert_eq!(read_decoder_block(&mut decoder, 16), [0.25, 0.5]);
        }
    }

    #[test]
    fn repeated_empty_decoder_reads_confirm_eof() {
        let mut decoder = IntermittentDecoder { reads: vec![None; EOF_CONFIRMATION_READS].into_iter() };

        assert!(read_decoder_block(&mut decoder, 16).is_empty());
    }

    #[test]
    fn byte_length_probe_restores_type_erased_reader_position() {
        let mut reader: Box<dyn ReadSeek> = Box::new(std::io::Cursor::new(vec![0_u8; 4096]));
        reader.seek(SeekFrom::Start(137)).unwrap();

        assert_eq!(stream_byte_len(&mut *reader), Ok(4096));
        assert_eq!(reader.stream_position().unwrap(), 137);
    }
}
