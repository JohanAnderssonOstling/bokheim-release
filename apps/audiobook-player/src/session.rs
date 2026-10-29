use super::*;
#[cfg(not(target_arch = "wasm32"))]
use library_backend::ThumbnailResolution;
#[cfg(not(target_arch = "wasm32"))]
use library_backend::library::AudioStreamControl;
#[cfg(target_arch = "wasm32")]
use library_backend::library::BrowserAudiobook;
use std::future::Future;
use std::pin::Pin;

/// Device-local playback restoration owned by the playback UI, not by a
/// library session or application backend.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ActiveAudiobook {
    pub version: u32,
    pub session_id: uuid::Uuid,
    pub locator: BookLocator,
    pub position_ms: u64,
    pub speed: f64,
    pub metadata: library_model::PlaybackMetadata,
}

impl ActiveAudiobook {
    pub const VERSION: u32 = 1;

    pub fn new(locator: BookLocator, position_ms: u64, speed: f64, metadata: library_model::PlaybackMetadata) -> Self {
        Self { version: Self::VERSION, session_id: uuid::Uuid::new_v4(), locator, position_ms, speed, metadata }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.version != Self::VERSION {
            return Err("unsupported active audiobook version".to_owned());
        }
        if !self.speed.is_finite() || self.speed <= 0.0 {
            return Err("invalid active audiobook speed".to_owned());
        }
        Ok(())
    }
}

/// Shown position, playing, buffering, rate and sleep label.
type ShownState = (Duration, bool, bool, f64, Option<SharedString>);

/// Playback resources and behavior, independent of focus, menus, and view layout.
/// The application controller retains this session when individual views disappear.
pub struct PlaybackSession {
    refresh_task: Option<gpui::Task<()>>,
    cover_task: Option<gpui::Task<()>>,
    restore_cover: Vec<u8>,
    #[cfg(target_os = "android")]
    stopped_position: Option<android::PositionSnapshot>,
    #[cfg(target_os = "android")]
    seek_source: Option<android::PositionSnapshot>,
    position_task: Option<gpui::Task<Result<(), String>>>,
    #[cfg(target_os = "android")]
    release: Option<Pin<Box<dyn Future<Output = Result<(), String>>>>>,
    #[cfg(not(target_arch = "wasm32"))]
    stream_control: Option<AudioStreamControl>,
    resolve_task: Option<gpui::Task<()>>,
    load_task: Option<gpui::Task<()>>,
    save_speed: SaveSpeed,
    playback_requested: bool,
    pending_seek: Option<Duration>,
    #[cfg(not(target_arch = "wasm32"))]
    active_track: Option<book_model::AudiobookTrack>,
    #[cfg(not(target_arch = "wasm32"))]
    tracks: Vec<book_model::AudiobookTrack>,
    origin: Option<(BookLocator, LibraryClient)>,
    source_pending: bool,
    pub(super) stopped: bool,
    pub(super) book: Option<Arc<AudiobookBook>>,
    pub(super) audio: Option<AudioEngine>,
    pub(super) position: Duration,
    pub(super) read_status: ChapterReadStatus,
    pub(super) rate: f64,
    pub(super) observed_playing: bool,
    observed_buffering: bool,
    /// What the dock showed after the last notification; see `shown_state`.
    shown: ShownState,
    pub(super) error: Option<String>,
    pub(super) sleep: Option<SleepPlan>,
    pub(super) position_updates: async_channel::Sender<PlaybackPosition>,
    #[cfg(all(not(target_os = "android"), not(target_arch = "wasm32")))]
    pub(super) last_saved_position: Option<Duration>,
}

impl PlaybackSession {
    #[cfg(not(target_arch = "wasm32"))]
    fn track_offset(&self) -> Duration {
        self.active_track.as_ref().map(|track| Duration::from_millis(track.start_ms)).unwrap_or_default()
    }

    /// Refreshes a device-local restore record only from a confirmed playback
    /// position. A pending decoder seek must not become a durable resume point.
    pub fn update_restoration_record(&self, record: &mut ActiveAudiobook) -> bool {
        let Some(book) = &self.book else {
            return false;
        };
        #[cfg(all(not(target_os = "android"), not(target_arch = "wasm32")))]
        let position = match &self.audio {
            Some(audio) => match audio.confirmed_position() {
                Some(position) => self.track_offset().saturating_add(position),
                None => return false,
            },
            None => self.position,
        };
        #[cfg(target_os = "android")]
        let position = match &self.audio {
            Some(audio) => match audio.confirmed_position() {
                Some(position) => self.track_offset().saturating_add(position),
                None => return false,
            },
            None => self.stopped_position.as_ref().map(|snapshot| self.track_offset().saturating_add(snapshot.last_position())).unwrap_or(self.position),
        };
        #[cfg(target_arch = "wasm32")]
        let position = match &self.audio {
            Some(audio) => match audio.confirmed_position() {
                Some(position) => position,
                None => return false,
            },
            None => self.position,
        };
        record.position_ms = position.min(book.duration).as_millis() as u64;
        record.speed = self.rate;
        record.metadata.title = book.title.clone();
        record.metadata.author = book.author.clone().unwrap_or_default();
        record.metadata.narrator = book.narrator.clone();
        record.metadata.duration_ms = book.duration.as_millis() as u64;
        record.metadata.cover = self.restore_cover.clone();
        record.metadata.chapters = book.chapters.clone();
        #[cfg(not(target_arch = "wasm32"))]
        {
            record.metadata.tracks = self.tracks.clone();
            record.metadata.format = if self.tracks.is_empty() { book_model::BookFormat::M4b } else { book_model::BookFormat::Mp3Folder };
        }
        true
    }

    /// Capture shutdown data without retaining an entity or borrowing App after
    /// an await. Android's final position is read after service release completes.
    pub fn final_restoration_snapshot(&self, mut record: ActiveAudiobook) -> Box<dyn FnOnce() -> ActiveAudiobook> {
        self.update_restoration_record(&mut record);
        #[cfg(target_os = "android")]
        let position = self.stopped_position.clone();
        #[cfg(target_os = "android")]
        let track_offset = self.track_offset();
        Box::new(move || {
            #[cfg(target_os = "android")]
            if let Some(position) = position {
                record.position_ms = (track_offset.saturating_add(position.last_position()).as_millis() as u64).min(record.metadata.duration_ms);
            }
            record
        })
    }

    pub fn restore(record: &ActiveAudiobook, library: LibraryClient, save_speed: SaveSpeed, cx: &mut Context<Self>) -> Self {
        let mut session = Self::create(record.locator.clone(), library, None, None, record.speed, save_speed, cx);
        session.playback_requested = false;
        session.position = Duration::from_millis(record.position_ms);
        let metadata = record.metadata.clone();
        #[cfg(target_arch = "wasm32")]
        let metadata = {
            let mut metadata = metadata;
            metadata.cover.clear();
            metadata
        };
        session.restore_cover = metadata.cover.clone();
        session.book = Some(Arc::new(Self::book_from_metadata(metadata)));
        #[cfg(all(not(target_os = "android"), not(target_arch = "wasm32")))]
        {
            session.last_saved_position = Some(session.position);
        }
        session
    }

    /// Rebuild a stopped session after an aborted removal, without opening media.
    /// Keep the entity identity so existing dock/full-player observers still work.
    pub fn recover_paused(&mut self, record: &ActiveAudiobook, cx: &mut Context<Self>) -> bool {
        if !self.stopped {
            return false;
        }
        let Some((_, library)) = self.origin.clone() else {
            return false;
        };
        let replacement = Self::restore(record, library, self.save_speed, cx);
        *self = replacement;
        cx.notify();
        true
    }

    fn book_from_metadata(metadata: library_model::PlaybackMetadata) -> AudiobookBook {
        let duration = Duration::from_millis(metadata.duration_ms);
        let mut chapters = metadata.chapters;
        if chapters.is_empty() {
            chapters.push(AudiobookChapter { title: metadata.title.clone(), start_ms: 0, end_ms: metadata.duration_ms });
        }
        AudiobookBook { title: metadata.title, author: Some(metadata.author), narrator: metadata.narrator, duration, chapters, cover: Self::cover_image(&metadata.cover) }
    }

    fn cover_image(bytes: &[u8]) -> Option<Arc<Image>> {
        let format = if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            ImageFormat::Jpeg
        } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            ImageFormat::Png
        } else if bytes.starts_with(b"BM") {
            ImageFormat::Bmp
        } else {
            return None;
        };
        Some(Arc::new(Image::from_bytes(format, bytes.to_vec())))
    }

    fn apply_cover(&mut self, bytes: Vec<u8>) -> bool {
        // Keep device-local restoration small even when source artwork is large.
        if self.stopped || bytes.len() > 256 * 1024 || bytes == self.restore_cover {
            return false;
        }
        let Some(cover) = Self::cover_image(&bytes) else {
            return false;
        };
        #[cfg(target_os = "android")]
        if let Some(audio) = &self.audio {
            if let Err(error) = audio.set_artwork(&bytes) {
                log::warn!("Could not update Android audiobook artwork: {error}");
            }
        }
        self.restore_cover = bytes;
        if let Some(book) = &mut self.book {
            Arc::make_mut(book).cover = Some(cover);
        }
        true
    }

    pub fn ensure_source(&mut self, cx: &mut Context<Self>) {
        if self.stopped || !(self.playback_requested || cfg!(target_os = "android") && self.pending_seek.is_some()) || self.audio.is_some() || self.source_pending {
            return;
        }
        let Some((locator, library)) = self.origin.clone() else {
            return;
        };
        let target = audiobook_toc_target(self.position.as_millis() as u64);
        self.error = None;
        self.start_load(locator, library, None, Some(target), cx);
    }

    /// Toggle transport intent from the dock.
    pub fn request_toggle(&mut self, cx: &mut Context<Self>) {
        self.release_failed_source();
        if self.transport_playing() {
            self.pause();
        } else {
            self.toggle_playback();
        }
        self.ensure_source(cx);
        cx.notify();
    }

    fn release_failed_source(&mut self) {
        #[cfg(target_arch = "wasm32")]
        if let Some(audio) = self.audio.as_ref().filter(|audio| audio.has_failed()) {
            // A failed media element cannot resume its exhausted range source.
            // Only this explicit user action releases and resolves a fresh one.
            self.position = audio.last_confirmed_position();
            self.audio.take();
            self.playback_requested = false;
            self.observed_playing = false;
        }
        #[cfg(not(target_arch = "wasm32"))]
        if !self.transport_playing() && self.stream_control.as_ref().is_some_and(|control| control.failure().is_some()) {
            if let Some(audio) = &self.audio {
                #[cfg(not(target_os = "android"))]
                {
                    self.position = self.track_offset().saturating_add(audio.last_confirmed_position());
                }
                #[cfg(target_os = "android")]
                {
                    self.position = self.track_offset().saturating_add(audio.position());
                    audio.stop();
                }
            }
            if let Some(control) = self.stream_control.take() {
                control.cancel();
            }
            self.audio.take();
            self.playback_requested = false;
            self.observed_playing = false;
        }
    }

    pub(super) fn transport_playing(&self) -> bool {
        self.is_playing() || self.is_buffering() || (self.source_pending && self.playback_requested)
    }

    /// Reopening the active book reuses its source and decoder. A target received
    /// while loading takes precedence over the asynchronously read saved position.
    pub fn activate(&mut self, target: Option<&str>) {
        if self.stopped {
            return;
        }
        self.release_failed_source();
        if let Some(position) = target.and_then(book_model::audiobook_toc_position) {
            self.seek_to(Duration::from_millis(position));
        }
        self.playback_requested = true;
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(control) = &self.stream_control {
            control.set_paused(false);
        }
        if self.audio.is_some() && !self.is_playing() && !self.is_buffering() {
            self.toggle_playback();
        }
    }

    pub(super) fn set_rate(&mut self, rate: f64, cx: &mut Context<Self>) {
        let applied = tempo_for(rate).and_then(|tempo| match self.audio.as_mut() {
            Some(audio) => audio.set_tempo(tempo),
            None => Ok(()),
        });
        match applied {
            Ok(()) => {
                self.rate = rate;
                (self.save_speed)(rate, cx);
            }
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    pub fn open(locator: BookLocator, library: LibraryClient, resolved: ResolvedBook, initial_target: Option<String>, speed: f64, save_speed: fn(f64, &mut App), cx: &mut Context<Self>) -> Self {
        Self::create(locator, library, Some(resolved), initial_target, speed, save_speed, cx)
    }

    fn create(locator: BookLocator, library: LibraryClient, resolved: Option<ResolvedBook>, initial_target: Option<String>, speed: f64, save_speed: SaveSpeed, cx: &mut Context<Self>) -> Self {
        let (position_updates, saved_positions) = async_channel::unbounded::<PlaybackPosition>();
        let persistence_library = library.clone();
        let persistence_hash = locator.content_hash();
        #[cfg(not(target_arch = "wasm32"))]
        let position_task = cx.background_executor().spawn(async move {
            let mut result = Ok(());
            while let Ok(update) = saved_positions.recv().await {
                result = match update {
                    PlaybackPosition::Position { position, progress } => persistence_library.update_reading_position(persistence_hash, book_model::audiobook_reading_position(position.as_millis() as u64), Some(progress), None).await,
                    PlaybackPosition::ResetReadStatus { target } => match target {
                        Some(target) => persistence_library.reset_chapter_read_status_at(persistence_hash, target).await.map(|_| ()),
                        None => persistence_library.reset_chapter_read_status(persistence_hash).await.map(|_| ()),
                    },
                };
                if let Err(error) = &result {
                    let title = persistence_library.book_detail(persistence_hash).await.ok().map(|detail| detail.book.title).unwrap_or_else(|| "Untitled".into());
                    log::warn!("failed to persist audiobook position for book {title:?}: {error}");
                }
            }
            result.map_err(|error| error.to_string())
        });

        #[cfg(target_arch = "wasm32")]
        let position_task = cx.spawn(async move |_, _| {
            let mut result = Ok(());
            while let Ok(update) = saved_positions.recv().await {
                result = match update {
                    PlaybackPosition::Position { position, progress } => persistence_library.update_reading_position(persistence_hash, book_model::audiobook_reading_position(position.as_millis() as u64), Some(progress), None).await,
                    PlaybackPosition::ResetReadStatus { target } => match target {
                        Some(target) => persistence_library.reset_chapter_read_status_at(persistence_hash, target).await.map(|_| ()),
                        None => persistence_library.reset_chapter_read_status(persistence_hash).await.map(|_| ()),
                    },
                };
                if let Err(error) = &result {
                    let title = persistence_library.book_detail(persistence_hash).await.ok().map(|detail| detail.book.title).unwrap_or_else(|| "Untitled".into());
                    log::warn!("failed to persist audiobook position for book {title:?}: {error}");
                }
            }
            result.map_err(|error| error.to_string())
        });

        #[cfg(feature = "kobo")]
        let (refresh_events, refresh_requests) = async_channel::bounded(1);
        #[cfg(feature = "kobo")]
        cx.observe_self(move |_, _| { let _ = refresh_events.try_send(()); }).detach();
        let timer = cx.background_executor().clone();
        let refresh_task = cx.spawn(async move |this, cx| {
            loop {
                #[cfg(feature = "kobo")]
                {
                    while refresh_requests.try_recv().is_ok() {}
                    let Ok(delay) = this.update(cx, |this, _| this.kobo_refresh_delay()) else { break };
                    match delay {
                        None => if refresh_requests.recv().await.is_err() { break; },
                        Some(delay) if delay <= Duration::from_millis(250) => timer.timer(delay).await,
                        Some(delay) => {
                            futures_util::future::select(Box::pin(timer.timer(delay)), Box::pin(refresh_requests.recv())).await;
                        }
                    }
                }
                #[cfg(not(feature = "kobo"))]
                timer.timer(Duration::from_millis(250)).await;
                if this
                    .update(cx, |this, cx| {
                        #[cfg(not(target_arch = "wasm32"))]
                        this.advance_track_if_finished(cx);
                        #[cfg(target_os = "android")]
                        {
                            let previous_target = this.seek_source.as_ref().and_then(android::PositionSnapshot::take_seek_request);
                            let target = if let Some(audio) = &this.audio {
                                let current_target = audio.take_seek_request();
                                if current_target.is_some() {
                                    this.playback_requested = audio.playback_requested();
                                }
                                // Retain the old state until Java has replaced its
                                // media item, so presses during loading are not lost.
                                if audio.confirmed_position().is_some() {
                                    this.seek_source = None;
                                }
                                current_target.or(previous_target)
                            } else {
                                previous_target
                            };
                            if let Some(target) = target {
                                this.seek_to(target);
                                this.finish_seek(cx);
                            }
                        }
                        if this.refresh_playback_state() {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        });

        let mut session = Self::new(speed, position_updates);
        session.refresh_task = Some(refresh_task);
        session.position_task = Some(position_task);
        session.save_speed = save_speed;
        session.origin = Some((locator.clone(), library.clone()));
        let status_library = library.clone();
        let status_hash = locator.content_hash();
        cx.spawn(async move |this, cx| {
            if let Ok(detail) = status_library.book_detail(status_hash).await {
                let _ = this.update(cx, |this, cx| {
                    this.read_status.completed.extend(detail.chapter_read_status.completed);
                    cx.notify();
                });
            }
        })
        .detach();
        if let Some(resolved) = resolved {
            session.start_load(locator, library, Some(resolved), initial_target, cx);
        }
        session
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn advance_track_if_finished(&mut self, cx: &mut Context<Self>) {
        if !self.playback_requested || !self.audio.as_ref().is_some_and(AudioEngine::finished) {
            return;
        }
        let Some(current) = &self.active_track else { return };
        let Some(next) = self.tracks.iter().find(|track| track.start_ms == current.end_ms).cloned() else { return };
        self.position = Duration::from_millis(next.start_ms);
        #[cfg(target_os = "android")]
        if let Some(audio) = &self.audio {
            audio.stop();
        }
        self.audio.take();
        if let Some(control) = self.stream_control.take() {
            control.cancel();
        }
        self.active_track = None;
        self.observed_playing = false;
        self.observed_buffering = false;
        self.ensure_source(cx);
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn load_source_at(&mut self, mut source: AudiobookSource, initial_position: Duration, cx: &mut Context<Self>) {
        self.stream_control = source.stream_control.clone();
        self.active_track = source.track.clone();
        self.tracks = source.cached_metadata.as_ref().map(|metadata| metadata.tracks.clone()).unwrap_or_default();
        if let Some(control) = &self.stream_control {
            control.set_paused(!self.playback_requested);
        }
        self.error = None;
        cx.notify();

        #[cfg(target_os = "android")]
        let position_updates = self.position_updates.clone();
        let work = cx.background_executor().spawn(async move {
            let _opening = OpeningTiming::new("load_source");
            let metadata_timing = OpeningTiming::new("metadata_read");
            let metadata = source.cached_metadata.take().ok_or("Audiobook metadata is not yet available")?;
            let mut chapters = metadata.chapters;
            let duration = Duration::from_millis(metadata.duration_ms);
            if chapters.is_empty() {
                chapters.push(AudiobookChapter { title: metadata.title.clone(), start_ms: 0, end_ms: metadata.duration_ms });
            }
            let book = AudiobookBook {
                title: metadata.title,
                author: Some(metadata.author),
                narrator: metadata.narrator,
                duration,
                chapters,
                cover: (!metadata.cover.is_empty()).then(|| Arc::new(Image::from_bytes(ImageFormat::Jpeg, metadata.cover))),
            };
            #[cfg(target_os = "android")]
            let track_start = source.track.as_ref().map(|track| Duration::from_millis(track.start_ms)).unwrap_or_default();
            let track_duration = source.track.as_ref().map(|track| Duration::from_millis(track.end_ms - track.start_ms)).unwrap_or(book.duration);
            #[cfg(target_os = "android")]
            let track_position = initial_position.saturating_sub(track_start).min(track_duration);
            drop(metadata_timing);
            let rewind_timing = OpeningTiming::new("rewind");
            source.reader.seek(SeekFrom::Start(0)).map_err(|error| format!("Could not rewind M4B: {error}"))?;
            drop(rewind_timing);
            #[cfg(target_os = "android")]
            let audio = AudioEngine::open(source.reader, source.playback_file, source.stream_control, &book, source.track.as_ref(), track_position, position_updates)?;
            #[cfg(all(not(target_os = "android"), not(target_arch = "wasm32")))]
            let audio = AudioEngine::open(source.reader, track_duration, source.stream_control).map_err(|error| format!("Could not open audiobook audio: {error}"))?;
            Ok::<_, String>((book, audio))
        });
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                this.finish_load(result, initial_position);
                if this.audio.is_none() {
                    this.ensure_source(cx);
                }
                cx.notify();
            });
        }));
    }

    #[cfg(target_arch = "wasm32")]
    fn load_source_at(&mut self, source: BrowserAudiobook, initial: Duration, cx: &mut Context<Self>) {
        let result = (|| {
            let metadata = &source.metadata;
            let book = AudiobookBook {
                title: metadata.title.clone(),
                author: Some(metadata.author.clone()).filter(|s| !s.is_empty()),
                narrator: metadata.narrator.clone(),
                duration: Duration::from_millis(metadata.duration_ms),
                chapters: metadata.chapters.clone(),
                cover: (!metadata.cover.is_empty()).then(|| Arc::new(Image::from_bytes(ImageFormat::Jpeg, metadata.cover.clone()))),
            };
            let audio = AudioEngine::open(source, &book, initial, self.position_updates.clone())?;
            Ok((book, audio))
        })();
        self.finish_load(result, initial);
        cx.notify();
    }

    fn start_load(&mut self, locator: BookLocator, library: LibraryClient, resolved: Option<ResolvedBook>, initial_target: Option<String>, cx: &mut Context<Self>) {
        self.source_pending = true;
        let content_hash = locator.content_hash();
        #[cfg(not(target_arch = "wasm32"))]
        if self.restore_cover.is_empty() {
            let cover_library = library.clone();
            self.cover_task = Some(cx.spawn(async move |this, cx| {
                if let Ok(Some(bytes)) = cover_library.thumbnail(content_hash, ThumbnailResolution::Browse).await {
                    let _ = this.update(cx, |session, cx| {
                        if session.apply_cover(bytes) {
                            cx.notify();
                        }
                    });
                }
            }));
        }
        self.resolve_task = Some(cx.spawn(async move |this, cx| {
            let result = async {
                let position_timing = OpeningTiming::new("saved_position_read");
                let saved_position = library.reading_position(content_hash).await.map_err(|error| format!("Could not load audiobook position: {error}"))?;
                drop(position_timing);
                let requested_position = initial_target.as_deref().and_then(book_model::audiobook_toc_position);
                let saved_position = saved_position.as_deref().and_then(book_model::audiobook_position_millis);
                let initial_position = Duration::from_millis(requested_position.or(saved_position).unwrap_or(0));
                #[cfg(not(target_arch = "wasm32"))]
                let cached_metadata = match library.audiobook_playback_metadata(content_hash).await {
                    Ok(metadata) => metadata,
                    Err(error) => {
                        log::warn!("Cached audiobook metadata unavailable: {error}");
                        None
                    }
                };
                #[cfg(not(target_arch = "wasm32"))]
                let selected_track = cached_metadata.as_ref().filter(|metadata| metadata.format == book_model::BookFormat::Mp3Folder).and_then(|metadata| {
                    let position = initial_position.as_millis() as u64;
                    metadata
                        .tracks
                        .iter()
                        .enumerate()
                        .find(|(_, track)| position >= track.start_ms && position < track.end_ms)
                        .or_else(|| metadata.tracks.last().map(|track| (metadata.tracks.len() - 1, track)))
                        .map(|(index, track)| (index, track.clone()))
                });
                #[cfg(not(target_arch = "wasm32"))]
                let resolved = if let Some((index, _)) = &selected_track {
                    library.resolve_audiobook_track(content_hash, *index).await.map_err(|error| error.to_string())?
                } else {
                    match resolved {
                        Some(resolved) => resolved,
                        None => library.resolve_book(content_hash).await.map_err(|error| error.to_string())?,
                    }
                };
                #[cfg(target_arch = "wasm32")]
                let resolved = match resolved {
                    Some(resolved) => resolved,
                    None => library.resolve_book(content_hash).await.map_err(|error| error.to_string())?,
                };
                let resolved = match resolved {
                    ResolvedBook::Audiobook(book) => book,
                    ResolvedBook::Document(_) => return Err("expected an audiobook".into()),
                };
                #[cfg(not(target_arch = "wasm32"))]
                let mut source = AudiobookSource::new(resolved.reader);
                #[cfg(not(target_arch = "wasm32"))]
                {
                    source.stream_control = resolved.stream_control;
                    source.track = selected_track.map(|(_, track)| track);
                }
                #[cfg(not(target_arch = "wasm32"))]
                {
                    source.cached_metadata = cached_metadata;
                }
                #[cfg(target_arch = "wasm32")]
                let source = resolved;
                #[cfg(target_os = "android")]
                let source = AudiobookSource { playback_file: resolved.playback_file, ..source };
                Ok::<_, String>((source, initial_position))
            }
            .await;
            let _ = this.update(cx, |this, cx| match result {
                Ok((source, initial_position)) => this.load_source_at(source, initial_position, cx),
                Err(error) => {
                    this.source_pending = false;
                    this.playback_requested = false;
                    this.pending_seek = None;
                    #[cfg(target_os = "android")]
                    if this.seek_source.take().is_some() { android::stop_detached_service(); }
                    this.error = Some(error);
                    cx.notify();
                }
            });
        }));
    }

    fn finish_load(&mut self, result: Result<(AudiobookBook, AudioEngine), String>, _initial_position: Duration) {
        if self.stopped {
            return;
        }
        self.source_pending = false;
        match result {
            Ok((book, mut audio)) => {
                #[cfg(target_os = "android")]
                if let Some(target) = self.seek_source.as_ref().and_then(android::PositionSnapshot::take_seek_request) {
                    self.pending_seek = Some(target);
                }
                let pending_seek = self.pending_seek.take();
                #[cfg(not(target_arch = "wasm32"))]
                if let (Some(track), Some(position)) = (&self.active_track, pending_seek) {
                    let millis = position.as_millis() as u64;
                    if millis < track.start_ms || (millis >= track.end_ms && position < book.duration) {
                        self.position = position;
                        self.pending_seek = Some(position);
                        self.book = Some(Arc::new(book));
                        return;
                    }
                }
                #[cfg(not(target_arch = "wasm32"))]
                let track_start = self.active_track.as_ref().map(|track| Duration::from_millis(track.start_ms)).unwrap_or_default();
                #[cfg(all(not(target_os = "android"), not(target_arch = "wasm32")))]
                let initial_position = pending_seek.unwrap_or(_initial_position).min(book.duration).saturating_sub(track_start);
                #[cfg(target_os = "android")]
                let initial_position = pending_seek.map(|position| position.min(book.duration).saturating_sub(track_start)).unwrap_or_default();
                #[cfg(target_arch = "wasm32")]
                let initial_position = pending_seek.unwrap_or(Duration::ZERO).min(book.duration);
                if (!initial_position.is_zero() || pending_seek.is_some())
                    && let Err(error) = audio.seek_to(initial_position)
                {
                    self.error = Some(format!("Could not restore audiobook position: {error}"));
                }
                // The saved speed is applied before playback is heard, so a book
                // never starts at 1× and then jumps.
                if let Ok(tempo) = tempo_for(self.rate)
                    && let Err(error) = audio.set_tempo(tempo)
                {
                    self.error = Some(error);
                }
                if self.playback_requested {
                    audio.play();
                }
                #[cfg(target_os = "android")]
                if !self.playback_requested {
                    audio.prepare_paused();
                }
                #[cfg(not(target_arch = "wasm32"))]
                let audio_position = track_start.saturating_add(audio.position());
                #[cfg(target_arch = "wasm32")]
                let audio_position = audio.position();
                self.position = clamp_position(audio_position, book.duration);
                if let Some(error) = audio.take_error() {
                    self.error = Some(error);
                }
                #[cfg(all(not(target_os = "android"), not(target_arch = "wasm32")))]
                {
                    self.last_saved_position = Some(self.position);
                }
                self.audio = Some(audio);
                let mut book = Arc::new(book);
                if let Some(cover) = Self::cover_image(&self.restore_cover) {
                    #[cfg(target_os = "android")]
                    if let Some(audio) = &self.audio {
                        if let Err(error) = audio.set_artwork(cover.bytes()) {
                            log::warn!("Could not restore Android audiobook artwork: {error}");
                        }
                    }
                    Arc::make_mut(&mut book).cover = Some(cover);
                    self.book = Some(book);
                } else {
                    let cover = book.cover.clone();
                    self.book = Some(book);
                    if let Some(cover) = cover {
                        self.apply_cover(cover.bytes().to_vec());
                    }
                }
            }
            Err(error) => {
                self.playback_requested = false;
                self.pending_seek = None;
                #[cfg(target_os = "android")]
                if self.seek_source.take().is_some() { android::stop_detached_service(); }
                self.error = Some(error);
            }
        }
    }

    pub(super) fn new(rate: f64, position_updates: async_channel::Sender<PlaybackPosition>) -> Self {
        Self {
            refresh_task: None,
            cover_task: None,
            restore_cover: Vec::new(),
            position_task: None,
            #[cfg(target_os = "android")]
            stopped_position: None,
            #[cfg(target_os = "android")]
            seek_source: None,
            #[cfg(target_os = "android")]
            release: None,
            #[cfg(not(target_arch = "wasm32"))]
            stream_control: None,
            resolve_task: None,
            load_task: None,
            save_speed: |_, _| {},
            playback_requested: true,
            pending_seek: None,
            #[cfg(not(target_arch = "wasm32"))]
            active_track: None,
            #[cfg(not(target_arch = "wasm32"))]
            tracks: Vec::new(),
            origin: None,
            source_pending: false,
            stopped: false,
            book: None,
            audio: None,
            position: Duration::ZERO,
            read_status: Default::default(),
            rate,
            observed_playing: false,
            observed_buffering: false,
            shown: (Duration::ZERO, false, false, rate, None),
            error: None,
            sleep: None,
            position_updates,
            #[cfg(all(not(target_os = "android"), not(target_arch = "wasm32")))]
            last_saved_position: None,
        }
    }

    pub fn stop_playback(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(control) = &self.stream_control {
            control.cancel();
        }
        self.resolve_task.take();
        self.load_task.take();
        self.cover_task.take();
        if self.stopped {
            return;
        }
        #[cfg(all(not(target_os = "android"), not(target_arch = "wasm32")))]
        if let Some(audio) = &self.audio {
            audio.pause();
            self.position = self.track_offset().saturating_add(audio.last_confirmed_position());
        }
        #[cfg(any(target_os = "android", target_arch = "wasm32"))]
        if let Some(audio) = &self.audio {
            audio.stop();
        }
        #[cfg(target_arch = "wasm32")]
        if let Some(audio) = &self.audio {
            self.position = audio.last_confirmed_position();
        }
        #[cfg(target_os = "android")]
        if let Some(audio) = &self.audio {
            self.stopped_position = Some(audio.position_snapshot());
            self.release = Some(audio.release_completion());
        }
        self.audio.take();
        self.persist_position(true);
        #[cfg(target_os = "android")]
        if self.seek_source.take().is_some() && self.audio.is_none() { android::stop_detached_service(); }
        self.stopped = true;
        self.playback_requested = false;
        self.pending_seek = None;
        self.sleep = None;
        self.observed_playing = false;
    }

    /// Release decoder, network reads and ALSA before the Kobo powers down its radios.
    /// Retain the book and confirmed position for an explicit Play after waking.
    #[cfg(feature = "kobo")]
    pub fn prepare_sleep(&mut self, cx: &mut Context<Self>) {
        if self.stopped { return; }
        self.stop_playback();
        self.stream_control = None;
        self.source_pending = false;
        self.observed_buffering = false;
        self.stopped = false;
        cx.notify();
    }

    #[cfg(feature = "kobo")]
    fn kobo_refresh_delay(&self) -> Option<Duration> {
        if self.playback_requested || self.is_playing() || self.source_pending
            || self.audio.as_ref().is_some_and(|audio| audio.confirmed_position().is_none())
        {
            return Some(Duration::from_millis(250));
        }
        self.sleep.and_then(SleepPlan::deadline).map(|deadline| deadline.saturating_duration_since(Instant::now()).min(Duration::from_secs(60)))
    }

    /// Stop all producers, close the position queue, and await its final write.
    /// Used before deleting the originating library and during normal shutdown.
    pub fn stop_and_flush(&mut self) -> Pin<Box<dyn Future<Output = Result<(), String>>>> {
        self.stop_playback();
        #[cfg(not(target_os = "android"))]
        self.position_updates.close();
        let task = self.position_task.take();
        #[cfg(target_os = "android")]
        let release = self.release.take();
        #[cfg(target_os = "android")]
        let updates = self.position_updates.clone();
        Box::pin(async move {
            #[cfg(target_os = "android")]
            {
                if let Some(release) = release {
                    if let Err(error) = release.await {
                        if let Some(task) = task {
                            task.detach();
                        }
                        return Err(error);
                    }
                }
                updates.close();
            }
            match task {
                Some(task) => task.await,
                None => Ok(()),
            }
        })
    }

    /// The playback state as the dock displays it. The refresh loop notifies
    /// only when this changes, so ticks that would redraw identical pixels
    /// draw nothing.
    fn shown_state(&self) -> ShownState {
        (shown_position(self.position), self.observed_playing, self.observed_buffering, self.rate, self.sleep_label())
    }

    pub(super) fn refresh_playback_state(&mut self) -> bool {
        #[cfg(all(not(target_os = "android"), not(target_arch = "wasm32")))]
        if let Some(audio) = &mut self.audio {
            audio.refresh_output();
        }
        if let (Some(audio), Some(book)) = (&self.audio, &self.book) {
            #[cfg(not(target_arch = "wasm32"))]
            let position = self.track_offset().saturating_add(audio.position());
            #[cfg(target_arch = "wasm32")]
            let position = audio.position();
            self.position = clamp_position(position, book.duration);
        }
        let chapters_changed = self.update_chapter_read_status();
        let playing = self.is_playing();
        #[cfg(all(not(target_os = "android"), not(target_arch = "wasm32")))]
        if self.audio.as_ref().is_some_and(AudioEngine::take_output_lost) {
            self.playback_requested = false;
            if let Some(control) = &self.stream_control {
                control.set_paused(true);
            }
        }
        if self.observed_playing && !playing && !self.is_buffering() {
            self.playback_requested = false;
            #[cfg(not(target_arch = "wasm32"))]
            if let Some(control) = &self.stream_control {
                control.set_paused(true);
            }
        }
        self.observed_playing = playing;
        self.observed_buffering = self.is_buffering();
        #[cfg(any(target_os = "android", target_arch = "wasm32"))]
        if let Some(audio) = &self.audio {
            self.rate = audio.rate();
        }
        let error = self.audio.as_ref().and_then(AudioEngine::take_error);
        let has_error = error.is_some();
        if let Some(error) = error {
            self.error = Some(error);
        }
        let slept = self.tick_sleep_timer();
        self.persist_position(has_error);
        // Compared with what was last shown rather than with the start of this
        // tick: the sleep countdown runs on the wall clock, even while paused.
        let shown = self.shown_state();
        let changed = shown != self.shown;
        self.shown = shown;
        has_error || slept || changed || chapters_changed
    }

    pub fn is_buffering(&self) -> bool {
        self.audio.as_ref().is_some_and(AudioEngine::is_buffering)
    }

    #[cfg(not(target_os = "android"))]
    pub(super) fn tick_sleep_timer(&mut self) -> bool {
        let Some(plan) = self.sleep else { return false };
        let chapters = self.book.as_ref().map(|book| book.chapters.as_slice()).unwrap_or_default();
        let remaining = sleep_remaining(plan, self.position, chapters, Instant::now(), self.rate);
        if let Some(audio) = &self.audio {
            audio.set_volume(sleep_gain(remaining));
        }
        if !remaining.is_zero() {
            return false;
        }
        self.sleep = None;
        self.pause();
        if let Some(audio) = &self.audio {
            audio.set_volume(1.0);
        }
        true
    }

    /// Android's service owns the timer while the activity is suspended.
    #[cfg(target_os = "android")]
    pub(super) fn tick_sleep_timer(&mut self) -> bool {
        if let Some((deadline, chapter_end)) = self.audio.as_ref().and_then(AudioEngine::take_sleep_reset) {
            self.sleep = deadline.map(SleepPlan::Until).or_else(|| {
                let end = chapter_end?;
                self.book.as_ref()?.chapters.iter().position(|chapter| chapter.end() == end).map(SleepPlan::EndOfChapter)
            });
        }
        if self.audio.as_ref().is_some_and(AudioEngine::take_sleep_finished) {
            self.sleep = None;
            return true;
        }
        false
    }

    pub(super) fn sleep_label(&self) -> Option<SharedString> {
        self.sleep.map(|plan| plan.label(Instant::now()))
    }

    pub(super) fn choose_sleep(&mut self, choice: SleepChoice, cx: &mut Context<Self>) {
        self.set_sleep(choice.plan(self.active_chapter_index()));
        cx.notify();
    }

    fn set_sleep(&mut self, plan: Option<SleepPlan>) {
        #[cfg(target_os = "android")]
        if let Some(audio) = &self.audio {
            let deadline = match plan {
                Some(SleepPlan::Until(deadline)) => Some(deadline.saturating_duration_since(Instant::now())),
                _ => None,
            };
            let chapter_end = match plan {
                Some(SleepPlan::EndOfChapter(index)) => self.book.as_ref().and_then(|book| book.chapters.get(index)).map(|chapter| chapter.end()),
                _ => None,
            };
            if let Err(error) = audio.set_sleep(deadline, chapter_end) {
                self.error = Some(error);
                return;
            }
        }
        #[cfg(not(target_os = "android"))]
        if let Some(audio) = &self.audio {
            audio.set_volume(1.0);
        }
        self.sleep = plan;
    }

    pub(super) fn pause(&mut self) {
        if self.stopped {
            return;
        }
        if let Some(audio) = &self.audio {
            #[cfg(not(target_os = "android"))]
            audio.pause();
            #[cfg(target_os = "android")]
            if let Err(error) = audio.pause() {
                self.error = Some(error);
                return;
            }
        }
        self.playback_requested = false;
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(control) = &self.stream_control {
            control.set_paused(true);
        }
        self.persist_position(true);
    }

    pub(super) fn toggle_playback(&mut self) {
        if self.stopped {
            return;
        }
        let Some(audio) = self.audio.as_mut() else {
            self.playback_requested = !self.playback_requested;
            #[cfg(not(target_arch = "wasm32"))]
            if let Some(control) = &self.stream_control {
                control.set_paused(!self.playback_requested);
            }
            return;
        };
        match audio.toggle() {
            Ok(()) => {
                self.playback_requested = audio.is_playing() || audio.is_buffering();
                self.error = None;
                self.persist_position(true);
            }
            Err(error) => self.error = Some(error),
        }
    }

    pub(super) fn seek_by(&mut self, seconds: f64) {
        let Some(book) = &self.book else { return };
        let position = relative_seek_position(self.position, seconds, book.duration);
        self.seek_to(position);
    }

    pub(super) fn seek_to(&mut self, position: Duration) {
        self.seek_to_inner(position, true);
    }

    fn seek_to_inner(&mut self, position: Duration, persist: bool) {
        self.update_chapter_read_status();
        if persist {
            self.persist_position(true);
        }
        if self.stopped {
            return;
        }
        if self.audio.is_none() {
            let position = self.book.as_ref().map(|book| clamp_position(position, book.duration)).unwrap_or(position);
            self.pending_seek = Some(position);
            if self.book.is_some() {
                self.position = position;
                if persist {
                    self.persist_position(true);
                }
            }
            return;
        }
        let Some(book) = &self.book else { return };
        let position = clamp_position(position, book.duration);
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(track) = &self.active_track {
            let millis = position.as_millis() as u64;
            if millis < track.start_ms || (millis >= track.end_ms && position < book.duration) {
                #[cfg(target_os = "android")]
                if let Some(audio) = &self.audio {
                    self.seek_source = Some(audio.position_snapshot());
                    audio.stop_for_seek();
                }
                self.audio.take();
                if let Some(control) = self.stream_control.take() {
                    control.cancel();
                }
                self.active_track = None;
                self.observed_playing = false;
                self.observed_buffering = false;
                self.position = position;
                self.pending_seek = Some(position);
                #[cfg(target_os = "android")]
                if persist {
                    let _ = self.position_updates.try_send(PlaybackPosition::new(position, playback_progress(position, book.duration)));
                }
                if persist {
                    self.persist_position(true);
                }
                return;
            }
        }
        let Some(audio) = self.audio.as_ref() else { return };
        #[cfg(not(target_arch = "wasm32"))]
        let engine_position = position.saturating_sub(self.track_offset());
        #[cfg(target_arch = "wasm32")]
        let engine_position = position;
        match audio.seek_to(engine_position) {
            Ok(()) => self.position = position,
            Err(error) => self.error = Some(error),
        }
    }

    /// The transport's seeks. Like `request_toggle`, each is a complete action:
    /// it moves, opens the source if playback wants one, and redraws.
    pub(super) fn skip(&mut self, seconds: f64, cx: &mut Context<Self>) {
        self.seek_by(seconds);
        self.finish_seek(cx);
    }

    /// Uses the same short navigation steps as the transport buttons.
    pub fn skip_backward(&mut self, cx: &mut Context<Self>) {
        self.skip(SKIP_BACK_SECONDS, cx);
    }

    pub fn skip_forward(&mut self, cx: &mut Context<Self>) {
        self.skip(SKIP_FORWARD_SECONDS, cx);
    }

    /// Live gesture updates reach the decoder immediately. Persisting every
    /// scroll sample would queue a database write for every finger movement.
    pub(super) fn scrub_to(&mut self, position: Duration, cx: &mut Context<Self>) {
        self.seek_to_inner(position, false);
        self.finish_seek(cx);
    }

    pub(super) fn finish_scrub(&mut self, cx: &mut Context<Self>) {
        self.persist_position(true);
        cx.notify();
    }

    pub(super) fn previous_chapter(&mut self, cx: &mut Context<Self>) {
        if let Some(position) = self.book.as_ref().map(|book| previous_chapter_position(&book.chapters, self.position)) {
            self.seek_to(position);
        }
        self.finish_seek(cx);
    }

    pub(super) fn next_chapter(&mut self, cx: &mut Context<Self>) {
        if let Some(position) = self.book.as_ref().map(|book| next_chapter_position(&book.chapters, self.position, book.duration)) {
            self.seek_to(position);
        }
        self.finish_seek(cx);
    }

    /// Seeks to the start of a chapter picked from the chapter list.
    pub(super) fn go_to_chapter(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(start) = self.book.as_ref().and_then(|book| book.chapters.get(index)).map(|chapter| chapter.start()) {
            self.seek_to(start);
        }
        self.finish_seek(cx);
    }

    /// Seeks to a fraction of the current chapter, as a scrub on the progress bar.
    pub(super) fn seek_in_chapter(&mut self, fraction: f32, cx: &mut Context<Self>) {
        let target = self.book.as_ref().and_then(|book| book.chapters.get(self.active_chapter_index())).map(|chapter| chapter_position_at_fraction(chapter.start(), chapter.end(), fraction));
        if let Some(target) = target {
            self.seek_to(target);
        }
        self.finish_seek(cx);
    }

    fn finish_seek(&mut self, cx: &mut Context<Self>) {
        self.update_chapter_read_status();
        self.ensure_source(cx);
        cx.notify();
    }

    pub fn is_playing(&self) -> bool {
        self.audio.as_ref().is_some_and(AudioEngine::is_playing)
    }

    /// Loading and buffering an explicitly started book also count as listening.
    #[cfg(feature = "kobo")]
    pub fn prevents_idle_sleep(&self) -> bool {
        self.transport_playing()
    }

    /// Where playback is and how far through the book that is, in per cent —
    /// live, for the playing book's detail page, which would otherwise show
    /// the last position saved to the library.
    pub fn listening_position(&self) -> Option<(Duration, f32)> {
        let book = self.book.as_ref()?;
        Some((self.position, playback_progress(self.position, book.duration)))
    }

    pub fn chapter_read_status(&self) -> ChapterReadStatus {
        self.read_status.clone()
    }

    fn update_chapter_read_status(&mut self) -> bool {
        let Some(book) = &self.book else { return false };
        let target = book.chapters.get(self.active_chapter_index()).map(|chapter| audiobook_toc_target(chapter.start_ms));
        let at_end = !book.duration.is_zero() && self.position >= book.duration;
        if !at_end && self.read_status.active_target() == target.as_deref() {
            return false;
        }
        let entries: Vec<_> = book.chapters.iter().map(|chapter| BookTocEntry { title: chapter.title.clone(), target: audiobook_toc_target(chapter.start_ms), children: Vec::new() }).collect();
        self.read_status.update(&entries, target.as_deref(), at_end)
    }

    pub fn reset_chapter_read_status(&mut self, cx: &mut Context<Self>) {
        self.reset_read_status(None, cx);
    }

    pub fn reset_chapter_read_status_at(&mut self, target: String, cx: &mut Context<Self>) {
        self.reset_read_status(Some(target), cx);
    }

    fn reset_read_status(&mut self, target: Option<String>, cx: &mut Context<Self>) {
        self.persist_position(true);
        if self.position_updates.try_send(PlaybackPosition::ResetReadStatus { target: target.clone() }).is_err() {
            self.error = Some("Could not reset read status".into());
        } else {
            if let Some(target) = target {
                self.read_status.completed.remove(&target);
            } else {
                self.read_status.reset();
            }
        }
        cx.notify();
    }

    pub(super) fn active_chapter_index(&self) -> usize {
        let Some(book) = &self.book else { return 0 };
        active_chapter_index(&book.chapters, self.position)
    }

    #[cfg(all(not(target_os = "android"), not(target_arch = "wasm32")))]
    pub(super) fn persist_position(&mut self, force: bool) {
        let Some(book) = &self.book else { return };
        let position = match &self.audio {
            Some(audio) => {
                let Some(position) = audio.confirmed_position() else { return };
                self.track_offset().saturating_add(position)
            }
            None => self.position,
        }
        .min(book.duration);
        if !position_save_required(self.last_saved_position, position, force) {
            return;
        }
        let progress = playback_progress(position, book.duration);
        if self.position_updates.try_send(PlaybackPosition::new(position, progress)).is_err() {
            log::warn!("audiobook position persistence stopped");
        }
        self.last_saved_position = Some(position);
    }

    #[cfg(any(target_os = "android", target_arch = "wasm32"))]
    pub(super) fn persist_position(&mut self, _force: bool) {}
}

impl Drop for PlaybackSession {
    fn drop(&mut self) {
        if !self.stopped {
            self.stop_playback();
        }
        // Ordinary view/session release must not cancel a queued final save.
        #[cfg(not(target_os = "android"))]
        self.position_updates.close();
        if let Some(task) = self.position_task.take() {
            task.detach();
        }
    }
}

#[cfg(all(test, feature = "kobo"))]
mod kobo_power_tests {
    use super::*;
    use gpui::{AppContext, TestAppContext};

    #[gpui::test]
    fn sleep_releases_pending_work_but_keeps_explicit_resume_available(cx: &mut TestAppContext) {
        let (updates, _) = async_channel::unbounded();
        let session = cx.new(|_| PlaybackSession::new(1.0, updates));
        session.update(cx, |session, cx| {
            let control = AudioStreamControl::default();
            session.stream_control = Some(control.clone());
            session.position = Duration::from_secs(42);
            session.source_pending = true;
            assert!(session.prevents_idle_sleep());
            session.prepare_sleep(cx);
            assert!(control.is_cancelled());
            assert!(!session.source_pending);
            assert!(!session.stopped);
            assert!(!session.playback_requested);
            assert!(!session.prevents_idle_sleep());
            assert_eq!(session.position, Duration::from_secs(42));
            assert_eq!(session.kobo_refresh_delay(), None);
            session.sleep = Some(SleepPlan::Until(Instant::now() + Duration::from_secs(120)));
            assert_eq!(session.kobo_refresh_delay(), Some(Duration::from_secs(60)));
            session.toggle_playback();
            assert!(session.playback_requested);
            assert_eq!(session.kobo_refresh_delay(), Some(Duration::from_millis(250)));
        });
    }
}
