use super::*;

/// Device-local playback restoration owned by the playback UI, not by a
/// library session or application backend.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ActiveAudiobook {
    pub version: u32,
    pub session_id: uuid::Uuid,
    pub locator: BookLocator,
    pub position_ms: u64,
    pub speed: f64,
    pub metadata: app::PlaybackMetadata,
}

impl ActiveAudiobook {
    pub const VERSION: u32 = 1;

    pub fn new(locator: BookLocator, position_ms: u64, speed: f64, metadata: app::PlaybackMetadata) -> Self {
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

/// Playback resources and behavior, independent of focus, menus, and view layout.
/// The application controller retains this session when individual views disappear.
pub struct PlaybackSession {
    refresh_task: Option<gpui::Task<()>>,
    cover_task: Option<gpui::Task<()>>,
    restore_cover: Vec<u8>,
    #[cfg(target_os = "android")]
    stopped_position: Option<android::PositionSnapshot>,
    position_task: Option<gpui::Task<Result<(), String>>>,
    #[cfg(target_os = "android")]
    release: Option<std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>>>>>,
    #[cfg(not(target_arch = "wasm32"))]
    stream_control: Option<app::AudioStreamControl>,
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
    pub(super) rate: f64,
    pub(super) observed_playing: bool,
    observed_buffering: bool,
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
        record.metadata.chapters = book.chapters.iter().map(|chapter| library_model::AudiobookChapter { title: chapter.title.clone(), start_ms: chapter.start.as_millis() as u64, end_ms: chapter.end.as_millis() as u64 }).collect();
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
        let replacement = Self::restore(record, library, self.save_speed.clone(), cx);
        *self = replacement;
        cx.notify();
        true
    }

    fn book_from_metadata(metadata: app::PlaybackMetadata) -> AudiobookBook {
        let duration = Duration::from_millis(metadata.duration_ms);
        let mut chapters: Vec<_> = metadata.chapters.into_iter().map(|chapter| AudiobookChapter { title: chapter.title, start: Duration::from_millis(chapter.start_ms), end: Duration::from_millis(chapter.end_ms) }).collect();
        if chapters.is_empty() {
            chapters.push(AudiobookChapter { title: metadata.title.clone(), start: Duration::ZERO, end: duration });
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
        self.restore_cover = bytes;
        if let Some(book) = &mut self.book {
            Arc::make_mut(book).cover = Some(cover);
        }
        true
    }

    pub fn ensure_source(&mut self, cx: &mut Context<Self>) {
        if self.stopped || !self.playback_requested || self.audio.is_some() || self.source_pending {
            return;
        }
        let Some((locator, library)) = self.origin.clone() else {
            return;
        };
        let target = book_model::audiobook_toc_target(self.position.as_millis() as u64);
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
        if self.audio.as_ref().is_some_and(AudioEngine::has_failed) {
            // A failed media element cannot resume its exhausted range source.
            // Only this explicit user action releases and resolves a fresh one.
            self.position = self.audio.as_ref().unwrap().last_confirmed_position();
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

    pub(super) fn can_play(&self) -> bool {
        !self.stopped && (self.audio.is_some() || self.origin.is_some())
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

    pub(super) fn set_rate(&mut self, rate: f64, cx: &mut App) {
        let tempo = match tempo_for(rate) {
            Ok(tempo) => tempo,
            Err(error) => {
                self.error = Some(error);
                return;
            }
        };
        let Some(audio) = self.audio.as_mut() else {
            self.rate = rate;
            (self.save_speed)(rate, cx);
            return;
        };
        match audio.set_tempo(tempo) {
            Ok(()) => {
                self.rate = rate;
                (self.save_speed)(rate, cx);
            }
            Err(error) => self.error = Some(error),
        }
    }

    pub fn open(locator: BookLocator, library: LibraryClient, resolved: ResolvedBook, initial_target: Option<String>, speed: f64, save_speed: Rc<dyn Fn(f64, &mut App)>, cx: &mut Context<Self>) -> Self {
        Self::create(locator, library, Some(resolved), initial_target, speed, save_speed, cx)
    }

    fn create(locator: BookLocator, library: LibraryClient, resolved: Option<ResolvedBook>, initial_target: Option<String>, speed: f64, save_speed: SaveSpeed, cx: &mut Context<Self>) -> Self {
        let (position_updates, saved_positions) = async_channel::unbounded::<PlaybackPosition>();
        let persistence_library = library.clone();
        let persistence_hash = locator.content_hash();
        #[cfg(not(target_arch = "wasm32"))]
        let position_task = cx.background_executor().spawn(async move {
            let mut result = Ok(());
            while let Ok((position, progress)) = saved_positions.recv().await {
                let serialized = book_model::audiobook_reading_position(position.as_millis() as u64);
                result = persistence_library.update_reading_position(persistence_hash, serialized, Some(progress), None).await;
                if let Err(error) = &result {
                    log::warn!("failed to persist audiobook position for book {persistence_hash}: {error}");
                }
            }
            result
        });

        #[cfg(target_arch = "wasm32")]
        let position_task = cx.spawn(async move |_, _| {
            let mut result = Ok(());
            while let Ok((position, progress)) = saved_positions.recv().await {
                let serialized = book_model::audiobook_reading_position(position.as_millis() as u64);
                result = persistence_library.update_reading_position(persistence_hash, serialized, Some(progress), None).await;
                if let Err(error) = &result {
                    log::warn!("failed to persist audiobook position: {error}");
                }
            }
            result
        });

        let timer = cx.background_executor().clone();
        let refresh_task = cx.spawn(async move |this, cx| {
            loop {
                timer.timer(Duration::from_millis(250)).await;
                if this
                    .update(cx, |this, cx| {
                        #[cfg(not(target_arch = "wasm32"))]
                        this.advance_track_if_finished(cx);
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
        if let Some(resolved) = resolved {
            session.start_load(locator, library, Some(resolved), initial_target, cx);
        }
        session
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn advance_track_if_finished(&mut self, cx: &mut Context<Self>) {
        if !self.playback_requested || !self.audio.as_ref().is_some_and(AudioEngine::finished) { return; }
        let Some(current) = &self.active_track else { return };
        let Some(next) = self.tracks.iter().find(|track| track.start_ms == current.end_ms).cloned() else { return };
        self.position = Duration::from_millis(next.start_ms);
        #[cfg(target_os = "android")]
        if let Some(audio) = &self.audio { audio.stop(); }
        self.audio.take();
        if let Some(control) = self.stream_control.take() { control.cancel(); }
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
            let mut chapters: Vec<_> = metadata.chapters.into_iter().map(|c| AudiobookChapter { title: c.title, start: Duration::from_millis(c.start_ms), end: Duration::from_millis(c.end_ms) }).collect();
            let duration = Duration::from_millis(metadata.duration_ms);
            if chapters.is_empty() {
                chapters.push(AudiobookChapter { title: metadata.title.clone(), start: Duration::ZERO, end: duration });
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
                if this.audio.is_none() && this.playback_requested { this.ensure_source(cx); }
                cx.notify();
            });
        }));
    }

    #[cfg(target_arch = "wasm32")]
    fn load_source_at(&mut self, source: app::BrowserAudiobook, initial: Duration, cx: &mut Context<Self>) {
        let result = (|| {
            let metadata = &source.metadata;
            let book = AudiobookBook {
                title: metadata.title.clone(),
                author: Some(metadata.author.clone()).filter(|s| !s.is_empty()),
                narrator: metadata.narrator.clone(),
                duration: Duration::from_millis(metadata.duration_ms),
                chapters: metadata.chapters.iter().map(|c| AudiobookChapter { title: c.title.clone(), start: Duration::from_millis(c.start_ms), end: Duration::from_millis(c.end_ms) }).collect(),
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
                if let Ok(Some(bytes)) = cover_library.thumbnail(content_hash, app::ThumbnailResolution::Browse).await {
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
                    metadata.tracks.iter().enumerate().find(|(_, track)| position >= track.start_ms && position < track.end_ms)
                        .or_else(|| metadata.tracks.last().map(|track| (metadata.tracks.len() - 1, track)))
                        .map(|(index, track)| (index, track.clone()))
                });
                #[cfg(not(target_arch = "wasm32"))]
                let resolved = if let Some((index, _)) = &selected_track {
                    library.resolve_audiobook_track(content_hash, *index).await?
                } else {
                    match resolved { Some(resolved) => resolved, None => library.resolve_book(content_hash).await? }
                };
                #[cfg(target_arch = "wasm32")]
                let resolved = match resolved {
                    Some(resolved) => resolved,
                    None => library.resolve_book(content_hash).await?,
                };
                let resolved = resolved.into_audiobook()?;
                #[cfg(not(target_arch = "wasm32"))]
                let mut source = AudiobookSource::new(resolved.reader, resolved.path.into());
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
                let pending_seek = self.pending_seek.take();
                #[cfg(not(target_arch = "wasm32"))]
                if let (Some(track), Some(position)) = (&self.active_track, pending_seek) {
                    let millis = position.as_millis() as u64;
                    if millis < track.start_ms || (millis >= track.end_ms && position < book.duration) {
                        self.position = position;
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
                self.book = Some(Arc::new(book));
                if let Some(cover) = Self::cover_image(&self.restore_cover) {
                    Arc::make_mut(self.book.as_mut().unwrap()).cover = Some(cover);
                } else if let Some(cover) = self.book.as_ref().and_then(|book| book.cover.as_ref()) {
                    self.apply_cover(cover.bytes().to_vec());
                }
            }
            Err(error) => {
                self.playback_requested = false;
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
            release: None,
            #[cfg(not(target_arch = "wasm32"))]
            stream_control: None,
            resolve_task: None,
            load_task: None,
            save_speed: Rc::new(|_, _| {}),
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
            rate,
            observed_playing: false,
            observed_buffering: false,
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
        self.stopped = true;
        self.playback_requested = false;
        self.pending_seek = None;
        self.sleep = None;
        self.observed_playing = false;
    }

    /// Stop all producers, close the position queue, and await its final write.
    /// Used before deleting the originating library and during normal shutdown.
    pub fn stop_and_flush(&mut self) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>>>> {
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

    pub(super) fn refresh_playback_state(&mut self) -> bool {
        let previous_state = (self.position, self.observed_playing, self.observed_buffering, self.rate, self.sleep_label());
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
        // The wall-clock countdown also changes while playback is paused.
        has_error || slept || matches!(self.sleep, Some(SleepPlan::Until(_))) || previous_state != (self.position, self.observed_playing, self.observed_buffering, self.rate, self.sleep_label())
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
                self.book.as_ref()?.chapters.iter().position(|chapter| chapter.end == end).map(SleepPlan::EndOfChapter)
            });
        }
        if self.audio.as_ref().is_some_and(AudioEngine::take_sleep_finished) {
            self.sleep = None;
            return true;
        }
        false
    }

    pub(super) fn sleep_label(&self) -> Option<SharedString> {
        match self.sleep? {
            SleepPlan::Until(deadline) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                let label = if remaining > Duration::from_secs(60) { format!("{} min", remaining.as_secs().div_ceil(60)) } else { format_time(remaining) };
                Some(label.into())
            }
            SleepPlan::EndOfChapter(_) => Some("Chapter".into()),
        }
    }

    pub(super) fn set_sleep(&mut self, plan: Option<SleepPlan>) {
        #[cfg(target_os = "android")]
        if let Some(audio) = &self.audio {
            let deadline = match plan {
                Some(SleepPlan::Until(deadline)) => Some(deadline.saturating_duration_since(Instant::now())),
                _ => None,
            };
            let chapter_end = match plan {
                Some(SleepPlan::EndOfChapter(index)) => self.book.as_ref().and_then(|book| book.chapters.get(index)).map(|chapter| chapter.end),
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
        if self.stopped {
            return;
        }
        if self.audio.is_none() {
            let position = self.book.as_ref().map(|book| clamp_position(position, book.duration)).unwrap_or(position);
            self.pending_seek = Some(position);
            if self.book.is_some() {
                self.position = position;
                self.persist_position(true);
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
                if let Some(audio) = &self.audio { audio.stop(); }
                self.audio.take();
                if let Some(control) = self.stream_control.take() { control.cancel(); }
                self.active_track = None;
                self.observed_playing = false;
                self.observed_buffering = false;
                self.position = position;
                self.pending_seek = Some(position);
                #[cfg(target_os = "android")]
                let _ = self.position_updates.try_send((position, playback_progress(position, book.duration)));
                self.persist_position(true);
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

    pub(super) fn previous_chapter(&mut self) {
        let Some(book) = &self.book else { return };
        let position = previous_chapter_position(&book.chapters, self.position);
        self.seek_to(position);
    }

    pub(super) fn next_chapter(&mut self) {
        let Some(book) = &self.book else { return };
        let position = next_chapter_position(&book.chapters, self.position, book.duration);
        self.seek_to(position);
    }

    pub fn is_playing(&self) -> bool {
        self.audio.as_ref().is_some_and(AudioEngine::is_playing)
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
        if self.position_updates.try_send((position, progress)).is_err() {
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

#[cfg(all(test, not(target_os = "android"), not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn late_cover_updates_book_and_restorable_bytes_once() {
        let (mut session, _) = session();
        let bytes = b"\x89PNG\r\n\x1a\ncover".to_vec();
        assert!(session.apply_cover(bytes.clone()));
        assert_eq!(session.restore_cover, bytes);
        assert_eq!(session.book.as_ref().unwrap().cover.as_ref().unwrap().bytes(), bytes);
        assert!(!session.apply_cover(bytes));
    }

    #[test]
    fn invalid_oversized_and_post_close_covers_are_ignored() {
        let (mut session, _) = session();
        assert!(!session.apply_cover(b"not an image".to_vec()));
        let mut oversized = vec![0; 256 * 1024 + 1];
        oversized[..3].copy_from_slice(&[0xff, 0xd8, 0xff]);
        assert!(!session.apply_cover(oversized));
        session.stop_playback();
        assert!(!session.apply_cover(vec![0xff, 0xd8, 0xff]));
        assert!(session.restore_cover.is_empty());
    }

    fn session() -> (PlaybackSession, async_channel::Receiver<PlaybackPosition>) {
        let (sender, receiver) = async_channel::unbounded();
        let mut session = PlaybackSession::new(1.25, sender);
        session.book = Some(Arc::new(AudiobookBook { title: "Book".into(), author: None, narrator: None, duration: Duration::from_secs(100), chapters: vec![], cover: None }));
        session.position = Duration::from_secs(42);
        (session, receiver)
    }

    #[test]
    fn stop_saves_once_and_drop_does_not_overwrite_it() {
        let (mut session, receiver) = session();
        session.stop_playback();
        let (position, _) = receiver.try_recv().unwrap();
        assert_eq!(position, Duration::from_secs(42));
        assert!(session.stopped);
        session.position = Duration::ZERO;
        session.stop_playback();
        drop(session);
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn drop_saves_live_session() {
        let (session, receiver) = session();
        drop(session);
        assert_eq!(receiver.try_recv().unwrap().0, Duration::from_secs(42));
    }

    #[test]
    fn final_snapshot_outlives_session_without_app_access() {
        let (mut session, _) = session();
        let record = ActiveAudiobook::new(
            BookLocator::new(app::LibraryId::new_v4(), "b".repeat(64).parse().unwrap()),
            0,
            1.0,
            app::PlaybackMetadata { format: book_model::BookFormat::M4b, title: "Old title".into(), author: String::new(), narrator: None, duration_ms: 100_000, chapters: vec![], tracks: vec![], cover: vec![] },
        );
        session.stop_playback();
        let snapshot = session.final_restoration_snapshot(record);
        drop(session);
        let record = snapshot();
        assert_eq!(record.position_ms, 42_000);
        assert_eq!(record.speed, 1.25);
        assert_eq!(record.metadata.title, "Book");
    }

    #[gpui::test]
    async fn stop_and_flush_waits_for_queued_and_final_positions(cx: &mut gpui::TestAppContext) {
        let (mut session, positions) = session();
        session.position_updates.try_send((Duration::from_secs(10), 10.0)).unwrap();
        let saved = Arc::new(std::sync::Mutex::new(Vec::new()));
        let writes = saved.clone();
        let (permit, gate) = async_channel::bounded(1);
        session.position_task = Some(cx.update(|cx| {
            cx.background_executor().spawn(async move {
                gate.recv().await.unwrap();
                while let Ok((position, _)) = positions.recv().await {
                    writes.lock().unwrap().push(position);
                }
                Ok(())
            })
        }));
        let mut flush = session.stop_and_flush();
        let mut poll = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(flush.as_mut().poll(&mut poll).is_pending());
        assert!(saved.lock().unwrap().is_empty());
        permit.send(()).await.unwrap();
        flush.await.unwrap();
        assert_eq!(*saved.lock().unwrap(), [Duration::from_secs(10), Duration::from_secs(42)]);
    }

    #[gpui::test]
    async fn stop_and_flush_reports_write_failure(cx: &mut gpui::TestAppContext) {
        let (mut session, _) = session();
        session.position_task = Some(cx.update(|cx| cx.background_executor().spawn(async { Err("disk is read-only".to_owned()) })));
        assert_eq!(session.stop_and_flush().await, Err("disk is read-only".to_owned()));
    }

    #[gpui::test]
    async fn ordinary_drop_does_not_cancel_the_final_position_write(cx: &mut gpui::TestAppContext) {
        let (mut session, positions) = session();
        let (permit, gate) = async_channel::bounded(1);
        let (written, received) = async_channel::bounded(1);
        session.position_task = Some(cx.update(|cx| {
            cx.background_executor().spawn(async move {
                gate.recv().await.unwrap();
                while let Ok((position, _)) = positions.recv().await {
                    written.send(position).await.unwrap();
                }
                Ok(())
            })
        }));
        drop(session);
        permit.send(()).await.unwrap();
        assert_eq!(received.recv().await.unwrap(), Duration::from_secs(42));
    }

    #[test]
    fn stop_clears_sleep_and_observed_playback() {
        let (mut session, _) = session();
        session.sleep = Some(SleepPlan::Until(Instant::now() + Duration::from_secs(60)));
        session.observed_playing = true;
        session.stop_playback();
        assert!(session.sleep.is_none());
        assert!(!session.observed_playing);
        assert!(!session.is_playing());
    }

    #[test]
    fn pause_during_load_clears_playback_intent() {
        let (mut session, _) = session();
        assert!(session.playback_requested);
        session.pause();
        assert!(!session.playback_requested);
        session.activate(None);
        assert!(session.playback_requested);
    }

    #[test]
    fn reactivation_keeps_a_healthy_loading_source_and_applies_the_new_target() {
        let (mut session, _) = session();
        let control = app::AudioStreamControl::default();
        control.set_paused(true);
        session.stream_control = Some(control.clone());
        session.source_pending = true;
        session.playback_requested = false;
        session.activate(Some(&book_model::audiobook_toc_target(80_000)));
        assert!(session.playback_requested);
        assert!(!control.is_paused());
        assert!(!control.is_cancelled());
        assert_eq!(session.pending_seek, Some(Duration::from_secs(80)));
        assert_eq!(session.position, Duration::from_secs(80));
        // The source handle is shared, not replaced by reactivation.
        control.cancel();
        assert!(session.stream_control.as_ref().unwrap().is_cancelled());
    }

    #[test]
    fn reopening_loading_book_keeps_latest_explicit_target() {
        let (mut session, _) = session();
        session.activate(Some(&book_model::audiobook_toc_target(10_000)));
        session.activate(Some(&book_model::audiobook_toc_target(20_000)));
        assert_eq!(session.pending_seek, Some(Duration::from_secs(20)));
        session.activate(None);
        assert_eq!(session.pending_seek, Some(Duration::from_secs(20)));
        session.stop_playback();
        session.activate(Some(&book_model::audiobook_toc_target(30_000)));
        assert_eq!(session.pending_seek, None);
        assert!(!session.playback_requested);
    }

    #[test]
    fn paused_deferred_seeks_accumulate_and_clamp_without_opening_audio() {
        let (mut session, _) = session();
        session.playback_requested = false;
        session.seek_by(30.0);
        assert_eq!(session.position, Duration::from_secs(72));
        session.seek_by(30.0);
        assert_eq!(session.position, Duration::from_secs(100));
        assert_eq!(session.pending_seek, Some(Duration::from_secs(100)));
        assert!(session.audio.is_none());
        assert!(!session.playback_requested);
    }

    #[gpui::test]
    fn restored_session_has_no_source_work_or_audio_until_play(cx: &mut gpui::TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let launch = app::host::BackendLaunch::initialize(app::AppDataLocation::native_path(directory.path())).unwrap();
        let (backend, _) = app::AppClient::start_native(launch).unwrap();
        // Intentionally absent library: restoration must not try to open it.
        let locator = BookLocator::new(app::LibraryId::new_v4(), "b".repeat(64).parse().unwrap());
        let record = ActiveAudiobook::new(
            locator.clone(),
            42_000,
            1.25,
            app::PlaybackMetadata { format: book_model::BookFormat::M4b, title: "Restored book".into(), author: "Author".into(), narrator: None, duration_ms: 100_000, chapters: vec![], tracks: vec![], cover: b"\x89PNG\r\n\x1a\ncover".to_vec() },
        );
        let library = backend.library(*locator.library_id());
        let playback = cx.new(|cx| PlaybackSession::restore(&record, library, Rc::new(|_, _| {}), cx));
        cx.run_until_parked();
        playback.read_with(cx, |session, _| {
            assert!(!session.playback_requested);
            assert!(!session.source_pending);
            assert!(session.resolve_task.is_none());
            assert!(session.load_task.is_none());
            assert!(session.cover_task.is_none());
            assert!(session.audio.is_none());
            assert!(session.error.is_none());
            assert_eq!(session.position, Duration::from_secs(42));
            assert_eq!(session.rate, 1.25);
            assert_eq!(session.book.as_ref().unwrap().title, "Restored book");
            assert_eq!(session.book.as_ref().unwrap().chapters.len(), 1);
            let mut snapshot = record.clone();
            assert!(session.update_restoration_record(&mut snapshot));
            assert_eq!(snapshot.position_ms, 42_000);
            assert_eq!(snapshot.metadata.cover, record.metadata.cover);
            assert_eq!(session.book.as_ref().unwrap().cover.as_ref().unwrap().bytes(), record.metadata.cover);
        });
        // Cancel the writer before it can submit to the intentionally absent
        // library, then exercise recovery on the same entity.
        playback.update(cx, |session, cx| {
            drop(session.stop_and_flush());
            assert!(session.recover_paused(&record, cx));
            assert!(!session.stopped);
            assert!(session.can_play());
            assert!(!session.playback_requested);
            assert!(session.audio.is_none());
            assert!(!session.source_pending);
            assert!(session.resolve_task.is_none());
            assert!(session.load_task.is_none());
            assert!(session.cover_task.is_none());
            assert!(session.refresh_task.is_some());
            assert_eq!(session.position, Duration::from_secs(42));
            assert_eq!(session.rate, 1.25);
            assert!(!session.recover_paused(&record, cx), "a live session cannot be replaced by recovery");
            drop(session.stop_and_flush());
        });
    }
}
