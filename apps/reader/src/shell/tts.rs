//! Shared narration state and playback controls.
use crate::settings::{NarrationPauses, NarrationVoice};
use audiobook_player::{PlaybackMenu, SleepChoice, SleepPlan, format_playback_rate, sleep_menu, speed_menu};
use components::playback_secondary_button;
use gpui::prelude::*;
use gpui::{AnyElement, App, Context, SharedString, Task, div};
use gpui_component::menu::DropdownMenu;
use gpui_component::{Disableable, Icon, IconName};
#[cfg(not(target_arch = "wasm32"))]
use reader_speech::Voice;
use reader_speech::{DesktopSpeech, Pauses, SpeechControl, SpeechEvent, SpeechPassage, supports_languages};
use std::time::Duration;
use ui_components::{self as components, PlaybackTransportAction};
use web_time::Instant;

#[derive(Default)]
pub(crate) struct Narration {
    pub service: Option<DesktopSpeech>,
    #[cfg(target_arch = "wasm32")]
    languages: Vec<String>,
    #[cfg(target_arch = "wasm32")]
    availability_watch: Option<reader_speech::AvailabilityWatch>,
    #[cfg(target_arch = "wasm32")]
    availability_task: Option<Task<()>>,
    pub generation: u64,
    pub playing: bool,
    pub paused: bool,
    pub needs_restart: bool,
    pub finished: bool,
    pub current: Option<usize>,
    last_started: Option<(u64, usize)>,
    pub status: String,
    pub unavailable_reason: Option<String>,
    pub events_task: Option<Task<()>>,
    pub navigation_task: Option<Task<()>>,
    pub sleep_task: Option<Task<()>>,
    pub sleep: Option<SleepPlan>,
    pub end_fragment: Option<usize>,
    /// Whether the dock is on screen. It opens when reading starts and stays,
    /// paused, stopped or finished, until its close button.
    pub dock_open: bool,
    rate: Option<f64>,
    pauses: NarrationPauses,
    voice: NarrationVoice,
    media_title: String,
    media_author: String,
    media_chapter: String,
    media_cover: Vec<u8>,
    #[cfg(target_os = "android")]
    volume_button_capture: Option<bool>,
}

/// The pause choices in menu order, with what each asks of the voice.
const PAUSE_CHOICES: [(NarrationPauses, &str, Pauses); 4] = [
    (NarrationPauses::Short, "Short pauses", Pauses { sentence_scale: 0.6, passage_gap: 0.0 }),
    (NarrationPauses::Natural, "Natural pauses", Pauses::NATURAL),
    (NarrationPauses::Long, "Long pauses", Pauses { sentence_scale: 1.5, passage_gap: 0.4 }),
    (NarrationPauses::Longer, "Longer pauses", Pauses { sentence_scale: 2.0, passage_gap: 0.8 }),
];

#[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
const VOICE_CHOICES: [(NarrationVoice, &str, &str, Voice); 2] = [(NarrationVoice::Alba, "Alba", "Alba · Scottish English", Voice::Alba), (NarrationVoice::George, "George", "George · British English", Voice::George)];

#[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
fn speech_voice(choice: NarrationVoice) -> Voice {
    VOICE_CHOICES.into_iter().find(|(voice, ..)| *voice == choice).map_or(Voice::Alba, |(.., voice)| voice)
}

#[cfg(target_os = "android")]
fn speech_voice(_choice: NarrationVoice) -> Voice {
    // Older saved preferences may still select George, which Android no longer bundles.
    Voice::Alba
}

fn speech_pauses(choice: NarrationPauses) -> Pauses {
    PAUSE_CHOICES.into_iter().find(|(pauses, ..)| *pauses == choice).map_or(Pauses::NATURAL, |(.., speech)| speech)
}

impl Narration {
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.media_title = title.into();
        self
    }
    fn sync_media_metadata(&self) {
        if let Some(service) = &self.service {
            service.set_title(&self.media_title);
            service.set_media_metadata(&self.media_author, &self.media_chapter, &self.media_cover);
        }
    }

    pub fn set_chapter(&mut self, chapter: String) {
        if self.media_chapter != chapter {
            self.media_chapter = chapter;
            self.sync_media_metadata();
        }
    }

    /// An active narration session reserves hardware volume keys for Android's
    /// media volume, including while paused or preparing the next passage,
    /// unless the reader explicitly uses them to advance a reading ruler.
    #[cfg(target_os = "android")]
    pub fn sync_volume_buttons(&mut self, ruler_active: bool, cx: &App) {
        let capture = ruler_active || !self.playing;
        if self.volume_button_capture != Some(capture) {
            self.volume_button_capture = Some(capture);
            cx.set_volume_button_capture(capture);
        }
    }

    pub fn for_languages(languages: &[String]) -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        let unavailable_reason = (!supports_languages(languages)).then(|| "This book’s declared language is not supported. Text to speech currently reads English.".into());
        #[cfg(target_arch = "wasm32")]
        let unavailable_reason = None;
        Self {
            unavailable_reason,
            #[cfg(target_arch = "wasm32")]
            languages: languages.to_vec(),
            ..Default::default()
        }
    }

    pub fn available(&self) -> bool {
        #[cfg(target_arch = "wasm32")]
        return reader_speech::available();
        #[cfg(not(target_arch = "wasm32"))]
        true
    }

    pub fn with_availability<C: TtsReader>(self, _cx: &mut Context<C>) -> Self {
        #[cfg(target_arch = "wasm32")]
        {
            let mut state = self;
            state.refresh_availability();
            let (watch, events) = reader_speech::AvailabilityWatch::new();
            state.availability_watch = Some(watch);
            state.availability_task = Some(_cx.spawn(async move |this, cx| {
                while events.recv().await.is_ok() {
                    if this
                        .update(cx, |reader, cx| {
                            let state = reader.narration_mut();
                            state.refresh_availability();
                            if !state.available() {
                                reader.close_tts_dock(cx);
                            }
                            cx.notify();
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            }));
            return state;
        }
        #[cfg(not(target_arch = "wasm32"))]
        self
    }

    #[cfg(target_arch = "wasm32")]
    fn refresh_availability(&mut self) {
        self.unavailable_reason = (!supports_languages(&self.languages)).then(|| "No browser voice is available for this book’s language.".into());
    }

    pub fn start(&mut self, text: Vec<SpeechPassage>, index: usize) -> Result<Option<async_channel::Receiver<SpeechEvent>>, String> {
        self.begin(index, |service| service.replace_passages_from(text, index))
    }

    pub fn start_items(&mut self, count: usize, index: usize, glyph_start: u32) -> Result<Option<async_channel::Receiver<SpeechEvent>>, String> {
        self.begin(index, |service| service.read_items_from(count, index, glyph_start))
    }

    fn begin(&mut self, index: usize, start: impl FnOnce(&DesktopSpeech) -> u64) -> Result<Option<async_channel::Receiver<SpeechEvent>>, String> {
        #[cfg(target_arch = "wasm32")]
        self.refresh_availability();
        if let Some(reason) = &self.unavailable_reason {
            return Err(reason.clone());
        }
        let events = if self.service.is_none() {
            let service = DesktopSpeech::new()?;
            let events = service.events();
            self.service = Some(service);
            Some(events)
        } else {
            None
        };
        let Some(service) = self.service.as_ref() else {
            return Err("Speech service is unavailable. Try starting read aloud again.".into());
        };
        service.set_title(&self.media_title);
        service.set_media_metadata(&self.media_author, &self.media_chapter, &self.media_cover);
        service.set_rate(self.rate() as f32);
        service.set_pauses(speech_pauses(self.pauses));
        #[cfg(not(target_arch = "wasm32"))]
        service.set_voice(speech_voice(self.voice));
        #[cfg(target_arch = "wasm32")]
        service.set_languages(&self.languages);
        self.generation = start(service);
        self.playing = true;
        self.paused = false;
        self.needs_restart = false;
        self.finished = false;
        self.current = Some(index);
        self.end_fragment = None;
        self.status = "Preparing text to speech…".into();
        Ok(events)
    }

    pub fn accepts(&self, event: &SpeechEvent) -> bool {
        let generation = match event {
            SpeechEvent::Requested { generation, .. } | SpeechEvent::Started { generation, .. } | SpeechEvent::Finished { generation } | SpeechEvent::Failed { generation, .. } | SpeechEvent::Remote { generation, .. } => *generation,
        };
        self.playing && generation == self.generation
    }

    pub fn started_item(&mut self, index: usize) -> bool {
        let item = (self.generation, index);
        let changed = self.last_started != Some(item);
        self.last_started = Some(item);
        self.current = Some(index);
        self.status = if self.paused { "Paused" } else { "Reading" }.into();
        changed
    }

    pub fn pause(&mut self) {
        if let Some(service) = &self.service {
            service.pause();
        }
        self.paused = true;
        self.status = "Paused".into();
    }

    pub fn resume(&mut self) {
        if let Some(service) = &self.service {
            service.resume();
        }
        self.paused = false;
        self.status = "Reading".into();
    }

    pub fn navigated(&mut self) {
        if let Some(service) = &self.service {
            self.generation = service.cancel();
        }
        self.needs_restart = true;
        self.finished = false;
        self.current = None;
        self.status = if self.paused { "Paused at the new page" } else { "Moving to the new page…" }.into();
    }

    pub fn stop(&mut self) {
        if let Some(service) = &self.service {
            service.stop();
        }
        self.navigation_task = None;
        self.sleep_task = None;
        self.sleep = None;
        self.end_fragment = None;
        self.needs_restart = false;
        self.playing = false;
        self.paused = false;
        self.finished = false;
        self.current = None;
        self.last_started = None;
        self.status = "Stopped".into();
    }
    /// Closing releases this reader's endpoint and listener, and cancels its
    /// navigation/sleep work. Dropping the endpoint also unloads its voice.
    pub fn close(&mut self) {
        self.stop();
        self.events_task = None;
        self.service = None;
        self.dock_open = false;
    }

    /// What the TTS button in the rail or bottom bar shows.
    pub fn button_mark(&self) -> components::ReaderRailMark {
        match (self.playing, self.paused) {
            (true, false) => components::ReaderRailMark::Active,
            (true, true) => components::ReaderRailMark::Paused,
            (false, _) => components::ReaderRailMark::None,
        }
    }

    pub fn rate(&self) -> f64 {
        self.rate.unwrap_or(1.0)
    }

    pub fn set_rate(&mut self, rate: f64) {
        self.rate = Some(rate);
        if let Some(service) = &self.service {
            self.generation = service.set_rate(rate as f32);
        }
    }

    pub fn with_rate(mut self, rate: f64) -> Self {
        self.rate = Some(rate);
        self
    }

    pub fn set_pauses(&mut self, pauses: NarrationPauses) {
        self.pauses = pauses;
        if let Some(service) = &self.service {
            self.generation = service.set_pauses(speech_pauses(pauses));
        }
    }

    pub fn with_pauses(mut self, pauses: NarrationPauses) -> Self {
        self.pauses = pauses;
        self
    }

    pub fn with_voice(mut self, voice: NarrationVoice) -> Self {
        self.voice = voice;
        self
    }

    #[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
    fn set_voice(&mut self, voice: NarrationVoice) {
        if self.voice == voice {
            return;
        }
        self.voice = voice;
        if let Some(service) = &self.service {
            self.generation = service.set_voice(speech_voice(voice));
            if self.playing && !self.paused {
                self.status = "Preparing the selected voice…".into();
            }
        }
    }

    pub fn skip_to(&mut self, index: usize) {
        if self.playing
            && !self.needs_restart
            && let Some(service) = &self.service
        {
            self.generation = service.skip_to(index);
            self.finished = false;
            self.current = Some(index);
        }
    }

    pub fn limit_to(&mut self, end: Option<usize>) {
        if self.end_fragment != end {
            self.end_fragment = end;
            if self.playing
                && !self.needs_restart
                && let Some(service) = &self.service
            {
                self.generation = service.set_end_fragment(end);
            }
            self.finished = false;
        }
    }

    fn sleep_label(&self) -> Option<SharedString> {
        self.sleep.map(|plan| plan.label(Instant::now()))
    }
}

pub(crate) fn adjacent_item(current: usize, count: usize, forward: bool) -> Option<usize> {
    if current >= count {
        return None;
    }
    if forward { current.checked_add(1).filter(|index| *index < count) } else { current.checked_sub(1) }
}

pub(crate) trait TtsReader: Sized + 'static {
    fn narration_mut(&mut self) -> &mut Narration;
    fn toggle_active_narration(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.narration_mut().playing {
            return false;
        }
        self.play_pause_tts(cx);
        true
    }

    fn chapter_key(&self, cx: &App) -> usize;
    fn chapter_label(&self, cx: &App) -> String {
        format!("Chapter {}", self.chapter_key(cx) + 1)
    }
    fn update_tts_chapter_limit(&mut self, _cx: &mut Context<Self>) {}
    fn skip_tts_element(&mut self, forward: bool, cx: &mut Context<Self>);
    fn sleep_tts(&mut self, cx: &mut Context<Self>) {
        let state = self.narration_mut();
        state.pause();
        state.sleep = None;
        state.sleep_task = None;
        state.limit_to(None);
        cx.notify();
    }
    fn start_tts(&mut self, cx: &mut Context<Self>);
    fn toggle_tts_pause(&mut self, cx: &mut Context<Self>);
    fn stop_tts(&mut self, cx: &mut Context<Self>);
    fn speech_event(&mut self, event: SpeechEvent, cx: &mut Context<Self>);
    fn remote_control(&mut self, command: SpeechControl, cx: &mut Context<Self>) {
        match command {
            SpeechControl::Pause => self.narration_mut().pause(),
            SpeechControl::Resume => {
                if self.narration_mut().paused {
                    self.toggle_tts_pause(cx);
                }
            }
            SpeechControl::Stop => self.stop_tts(cx),
            SpeechControl::Item { generation, index } => {
                let state = self.narration_mut();
                state.generation = generation;
                state.finished = false;
                state.current = Some(index);
                state.status = if state.paused { "Paused" } else { "Reading" }.into();
            }
        }
        cx.notify();
    }
    /// The navigation button toggles the whole narration session and dock.
    fn press_tts(&mut self, cx: &mut Context<Self>) {
        if self.narration_mut().dock_open || self.narration_mut().playing {
            self.close_tts_dock(cx);
        } else {
            self.play_pause_tts(cx);
        }
    }

    /// The dock transport keeps its session open when paused or resumed.
    fn play_pause_tts(&mut self, cx: &mut Context<Self>) {
        if self.narration_mut().playing {
            self.toggle_tts_pause(cx);
        } else {
            self.narration_mut().dock_open = true;
            self.start_tts(cx);
        }
        cx.notify();
    }

    /// The dock's close button stops reading and takes the dock away.
    fn close_tts_dock(&mut self, cx: &mut Context<Self>) {
        self.stop_tts(cx);
        self.narration_mut().close();
        cx.notify();
    }
}

#[cfg(target_os = "android")]
pub(crate) fn load_media_metadata<C: TtsReader>(library: std::rc::Rc<library_backend::LibraryClient>, locator: library_model::BookLocator, cx: &mut Context<C>) {
    cx.spawn(async move |this, cx| {
        let detail = library.book_detail(locator.content_hash()).await.ok();
        let cover = library.thumbnail(locator.content_hash(), library_backend::ThumbnailResolution::Browse).await.ok().flatten().unwrap_or_default();
        let _ = this.update(cx, |reader, _| {
            let state = reader.narration_mut();
            if let Some(detail) = detail {
                state.media_title = detail.book.title;
                state.media_author = detail.authors.iter().map(|author| author.name.as_str()).collect::<Vec<_>>().join(", ");
            }
            state.media_cover = cover;
            state.sync_media_metadata();
        });
    })
    .detach();
}

pub(crate) fn listen<C: TtsReader>(events: async_channel::Receiver<SpeechEvent>, cx: &mut Context<C>) -> Task<()> {
    cx.spawn(async move |this, cx| {
        while let Ok(event) = events.recv().await {
            if this
                .update(cx, |reader, cx| {
                    reader.speech_event(event, cx);
                    let chapter = reader.chapter_label(cx);
                    reader.narration_mut().set_chapter(chapter);
                })
                .is_err()
            {
                break;
            }
        }
    })
}

fn choose_sleep<C: TtsReader>(reader: &mut C, choice: SleepChoice, cx: &mut Context<C>) {
    let chapter = reader.chapter_key(cx);
    let plan = choice.plan(chapter);
    let state = reader.narration_mut();
    state.sleep_task = None;
    state.sleep = plan;
    reader.update_tts_chapter_limit(cx);
    if let Some(until) = plan.and_then(SleepPlan::deadline) {
        reader.narration_mut().sleep_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let keep_waiting = this
                    .update(cx, |reader, cx| {
                        if reader.narration_mut().sleep.and_then(SleepPlan::deadline) != Some(until) {
                            return false;
                        }
                        if Instant::now() >= until {
                            reader.sleep_tts(cx);
                            return false;
                        }
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !keep_waiting {
                    break;
                }
            }
        }));
    }
    cx.notify();
}

/// The narration dock under the page, laid out like the audiobook dock but
/// with transport controls in the centre and tools alongside them on desktop.
/// Compact docks distribute every control with equal inner and outer spacing.
/// Every compact option opens a window-owned bottom sheet.
pub(crate) fn dock<C: TtsReader>(state: &Narration, compact: bool, theme: components::BrowserTheme, cx: &mut Context<C>) -> AnyElement {
    let playing = state.playing && !state.paused;
    let eligible = state.unavailable_reason.is_none();
    let status = state.unavailable_reason.clone().unwrap_or_else(|| if state.status.is_empty() { "Start reading".into() } else { state.status.clone() });
    let skippable = state.playing && !state.needs_restart;
    let transport = components::playback_transport_buttons("tts", playing, components::TransportLayout::Dock, None, components::PlaybackSkipKind::Elements, theme)
        .into_iter()
        .map(|(action, button)| match action {
            PlaybackTransportAction::PlayPause => button.disabled(!eligible).tooltip(status.clone()).on_click(cx.listener(|reader, _, _, cx| reader.play_pause_tts(cx))),
            PlaybackTransportAction::Back | PlaybackTransportAction::Previous => button.disabled(!skippable).on_click(cx.listener(|reader, _, _, cx| reader.skip_tts_element(false, cx))),
            PlaybackTransportAction::Forward | PlaybackTransportAction::Next => button.disabled(!skippable).on_click(cx.listener(|reader, _, _, cx| reader.skip_tts_element(true, cx))),
        })
        .collect::<Vec<_>>();

    let rate = state.rate();
    let entity = cx.entity().downgrade();
    let set_rate = move |rate, cx: &mut App| {
        let _ = entity.update(cx, |reader, cx| {
            reader.narration_mut().set_rate(rate);
            crate::settings::ReaderSettings::update(cx, |preferences| preferences.audiobook_speed = rate);
            cx.notify();
        });
    };
    let speed = components::player_dock_text_button("tts-speed", format_playback_rate(rate)).tooltip("Reading speed");
    let speed = if compact {
        speed.on_click(move |_, window, cx| speed_menu(rate).title("Reading speed").open_sheet(window, cx, set_rate.clone())).into_any_element()
    } else {
        speed.dropdown_menu_with_anchor(gpui::Anchor::BottomRight, move |menu, _, _| speed_menu(rate).dropdown(menu, set_rate.clone())).into_any_element()
    };

    #[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
    let voice = {
        let current_voice = state.voice;
        let voice_label = VOICE_CHOICES.into_iter().find(|(voice, ..)| *voice == current_voice).map_or("Alba", |(_, label, ..)| label);
        let entity = cx.entity().downgrade();
        let set_voice = move |choice, cx: &mut App| {
            let _ = entity.update(cx, |reader, cx| {
                reader.narration_mut().set_voice(choice);
                crate::settings::ReaderSettings::update(cx, |preferences| preferences.narration_voice = choice);
                cx.notify();
            });
        };
        let voice = components::player_dock_text_button("tts-voice", voice_label).tooltip("Narration voice");
        if compact {
            voice
                .on_click(move |_, window, cx| {
                    let choices = VOICE_CHOICES.into_iter().map(|(choice, _, label, _)| (choice, label.into(), choice == current_voice));
                    PlaybackMenu::new("tts-voice", "Narration voice", choices).open_sheet(window, cx, set_voice.clone());
                })
                .into_any_element()
        } else {
            voice
                .dropdown_menu_with_anchor(gpui::Anchor::BottomRight, move |mut menu, _, _| {
                    for (choice, _, label, _) in VOICE_CHOICES {
                        let set_voice = set_voice.clone();
                        menu = menu.item(components::menu_item(label, move |_, _, cx| set_voice(choice, cx)).checked(choice == current_voice));
                    }
                    menu
                })
                .into_any_element()
        }
    };

    let current_pauses = state.pauses;
    let entity = cx.entity().downgrade();
    let set_pauses = move |choice, cx: &mut App| {
        let _ = entity.update(cx, |reader, cx| {
            reader.narration_mut().set_pauses(choice);
            crate::settings::ReaderSettings::update(cx, |preferences| preferences.narration_pauses = choice);
            cx.notify();
        });
    };
    let pauses = playback_secondary_button(components::outlined_icon_button("tts-pauses", "Pauses between sentences and paragraphs", Icon::empty().path("icons/hourglass.svg")));
    let pauses = if compact {
        pauses
            .on_click(move |_, window, cx| {
                let choices = PAUSE_CHOICES.into_iter().map(|(choice, label, _)| (choice, label.into(), choice == current_pauses));
                PlaybackMenu::new("tts-pauses", "Pauses", choices).open_sheet(window, cx, set_pauses.clone());
            })
            .into_any_element()
    } else {
        pauses
            .dropdown_menu_with_anchor(gpui::Anchor::BottomRight, move |mut menu, _, _| {
                for (choice, label, _) in PAUSE_CHOICES {
                    let set_pauses = set_pauses.clone();
                    menu = menu.item(components::menu_item(label, move |_, _, cx| set_pauses(choice, cx)).checked(choice == current_pauses));
                }
                menu
            })
            .into_any_element()
    };

    let sleep_label = state.sleep_label();
    let sleep = match sleep_label.clone() {
        Some(label) => components::player_dock_text_button("tts-sleep", label).tooltip("Sleep timer"),
        None => components::outlined_icon_button("tts-sleep", "Sleep timer", Icon::empty().path("icons/alarm.svg")),
    };
    let entity = cx.entity().downgrade();
    let set_sleep = move |choice, cx: &mut App| {
        let _ = entity.update(cx, |reader, cx| choose_sleep(reader, choice, cx));
    };
    let sleep = playback_secondary_button(sleep).when(sleep_label.is_some(), |button| button.text_color(theme.text_accent));
    let sleep = if compact {
        sleep.on_click(move |_, window, cx| sleep_menu(sleep_label.as_deref()).open_sheet(window, cx, set_sleep.clone())).into_any_element()
    } else {
        sleep.dropdown_menu_with_anchor(gpui::Anchor::BottomLeft, move |menu, _, _| sleep_menu(sleep_label.as_deref()).dropdown(menu, set_sleep.clone())).into_any_element()
    };
    let close = playback_secondary_button(components::outlined_icon_button("tts-close", "Close player", IconName::Close)).on_click(cx.listener(|reader, _, _, cx| reader.close_tts_dock(cx)));

    if compact {
        let mut controls = Vec::new();
        #[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
        controls.push(voice);
        controls.extend([speed, pauses]);
        controls.extend(transport.into_iter().map(|button| button.into_any_element()));
        controls.extend([sleep, close.into_any_element()]);
        return components::player_dock_evenly(controls, theme).id("tts-dock").on_scroll_wheel(|_, _, cx| cx.stop_propagation()).into_any_element();
    }

    let trailing = components::player_dock_tools();
    #[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
    let trailing = trailing.child(voice);
    let trailing = trailing.child(sleep).child(speed).child(pauses).child(close);

    let transport = components::player_dock_transport(None, transport, theme);
    components::player_dock(div(), transport, trailing, theme).id("tts-dock").on_scroll_wheel(|_, _, cx| cx.stop_propagation()).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext, TestAppContext};

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn saved_voice_preferences_use_a_bundled_voice() {
        assert_eq!(speech_voice(NarrationVoice::Alba), Voice::Alba);
        let expected = if cfg!(target_os = "android") { Voice::Alba } else { Voice::George };
        assert_eq!(speech_voice(NarrationVoice::George), expected);
    }

    #[test]
    fn highlighting_changes_once_per_item_and_restarts_for_new_generations() {
        let mut state = Narration { generation: 7, current: Some(2), status: "Preparing text to speech…".into(), ..Default::default() };
        assert!(state.started_item(2), "the first chunk must highlight even though start already selected this item");
        assert_eq!(state.status, "Reading");
        assert!(!state.started_item(2), "later chunks must not repeat whole-item highlighting");
        assert!(state.started_item(3));
        state.generation = 8;
        state.paused = true;
        assert!(state.started_item(3), "a new generation follows the item again");
        assert_eq!(state.status, "Paused");
        assert!(!state.started_item(3));
    }

    struct PanelReader {
        tts: Narration,
    }

    impl TtsReader for PanelReader {
        fn narration_mut(&mut self) -> &mut Narration {
            &mut self.tts
        }
        fn chapter_key(&self, _: &App) -> usize {
            0
        }
        fn skip_tts_element(&mut self, _: bool, _: &mut Context<Self>) {}
        fn start_tts(&mut self, _: &mut Context<Self>) {
            self.tts.playing = true;
            self.tts.paused = false;
        }
        fn toggle_tts_pause(&mut self, _: &mut Context<Self>) {
            if self.tts.paused {
                self.tts.resume();
            } else {
                self.tts.pause();
            }
        }
        fn stop_tts(&mut self, _: &mut Context<Self>) {
            self.tts.stop();
        }
        fn speech_event(&mut self, _: SpeechEvent, _: &mut Context<Self>) {}
    }

    #[gpui::test]
    fn notification_controls_update_the_dock_and_reject_old_audio(cx: &mut TestAppContext) {
        let reader = cx.new(|_| PanelReader { tts: Narration { generation: 7, playing: true, ..Default::default() } });
        reader.update(cx, |reader, cx| {
            reader.remote_control(SpeechControl::Pause, cx);
            assert!(reader.tts.paused);
            reader.remote_control(SpeechControl::Resume, cx);
            assert!(!reader.tts.paused);
            reader.remote_control(SpeechControl::Item { generation: 8, index: 2 }, cx);
            assert_eq!(reader.tts.current, Some(2));
            assert!(!reader.tts.accepts(&SpeechEvent::Started { generation: 7, index: 0, glyph_ranges: vec![0..1] }));
            assert!(reader.tts.accepts(&SpeechEvent::Started { generation: 8, index: 0, glyph_ranges: vec![0..1] }));
            reader.remote_control(SpeechControl::Stop, cx);
            assert!(!reader.tts.playing);
            assert!(!reader.tts.accepts(&SpeechEvent::Started { generation: 8, index: 0, glyph_ranges: vec![0..1] }));
        });
    }

    #[gpui::test]
    fn drawing_the_dock_while_the_reader_is_borrowed_does_not_panic(cx: &mut TestAppContext) {
        let reader = cx.new(|_| PanelReader { tts: Narration::default() });
        reader.update(cx, |reader, cx| {
            let _dock = dock(&reader.tts, false, components::BrowserTheme::default(), cx);
            assert!(reader.tts.service.is_none(), "drawing the dock must not start synthesis");
            reader.tts = Narration::for_languages(&["sv".into()]);
            let _dock = dock(&reader.tts, true, components::BrowserTheme::default(), cx);
            assert!(reader.tts.service.is_none());
        });
    }

    #[gpui::test]
    fn dock_stays_when_paused_or_stopped_until_closed(cx: &mut TestAppContext) {
        let reader = cx.new(|_| PanelReader { tts: Narration::default() });
        reader.update(cx, |reader, cx| {
            reader.press_tts(cx);
            assert!(reader.tts.dock_open && reader.tts.playing);
            reader.play_pause_tts(cx);
            assert!(reader.tts.dock_open && reader.tts.paused);
            reader.play_pause_tts(cx);
            assert!(reader.tts.dock_open && !reader.tts.paused);
            reader.stop_tts(cx);
            assert!(reader.tts.dock_open && !reader.tts.playing);
            reader.close_tts_dock(cx);
            assert!(!reader.tts.dock_open && !reader.tts.playing);
        });
    }

    #[gpui::test]
    fn playback_key_only_toggles_an_active_narration_session(cx: &mut TestAppContext) {
        let reader = cx.new(|_| PanelReader { tts: Narration::default() });
        reader.update(cx, |reader, cx| {
            assert!(!reader.toggle_active_narration(cx));
            assert!(!reader.tts.playing && !reader.tts.dock_open);
            reader.press_tts(cx);
            assert!(reader.toggle_active_narration(cx));
            assert!(reader.tts.playing && reader.tts.paused && reader.tts.dock_open);
            assert!(reader.toggle_active_narration(cx));
            assert!(reader.tts.playing && !reader.tts.paused);
            reader.stop_tts(cx);
            assert!(!reader.toggle_active_narration(cx));
        });
    }

    #[gpui::test]
    fn navigation_tts_button_closes_playing_and_paused_sessions_and_cancels_work(cx: &mut TestAppContext) {
        let reader = cx.new(|_| PanelReader { tts: Narration::default() });
        for paused in [false, true] {
            reader.update(cx, |reader, cx| {
                reader.press_tts(cx);
                if paused {
                    reader.play_pause_tts(cx);
                }
                choose_sleep(reader, SleepChoice::Minutes(5), cx);
                reader.tts.navigation_task = Some(cx.spawn(async |_, _| std::future::pending().await));
                reader.tts.events_task = Some(cx.spawn(async |_, _| std::future::pending().await));
                reader.press_tts(cx);
                assert!(!reader.tts.dock_open && !reader.tts.playing && !reader.tts.paused);
                assert!(reader.tts.service.is_none() && reader.tts.events_task.is_none());
                assert!(reader.tts.navigation_task.is_none() && reader.tts.sleep_task.is_none() && reader.tts.sleep.is_none());
                assert!(!reader.tts.accepts(&SpeechEvent::Finished { generation: reader.tts.generation }));
            });
        }
    }

    #[gpui::test]
    fn closing_reader_cancels_sleep_navigation_and_event_work(cx: &mut TestAppContext) {
        let reader = cx.new(|_| PanelReader { tts: Narration { playing: true, paused: true, needs_restart: true, ..Default::default() } });
        reader.update(cx, |reader, cx| {
            choose_sleep(reader, SleepChoice::Minutes(5), cx);
            assert!(reader.tts.sleep_task.is_some());
            reader.tts.navigation_task = Some(cx.spawn(async |_, _| std::future::pending().await));
            reader.tts.events_task = Some(cx.spawn(async |_, _| std::future::pending().await));
            reader.tts.close();
            assert!(!reader.tts.playing && !reader.tts.paused && !reader.tts.needs_restart);
            assert!(reader.tts.service.is_none());
            assert!(reader.tts.sleep_task.is_none() && reader.tts.sleep.is_none());
            assert!(reader.tts.navigation_task.is_none() && reader.tts.events_task.is_none());
            assert!(!reader.tts.accepts(&SpeechEvent::Finished { generation: reader.tts.generation }));
        });
    }

    #[test]
    fn item_skips_stop_at_the_batch_boundaries() {
        assert_eq!(adjacent_item(1, 3, true), Some(2));
        assert_eq!(adjacent_item(1, 3, false), Some(0));
        assert_eq!(adjacent_item(0, 3, false), None);
        assert_eq!(adjacent_item(2, 3, true), None);
        assert_eq!(adjacent_item(0, 0, true), None);
    }

    #[gpui::test]
    fn sleep_pauses_at_the_current_element_and_can_be_cancelled(cx: &mut TestAppContext) {
        let reader = cx.new(|_| PanelReader { tts: Narration { playing: true, current: Some(4), ..Default::default() } });
        reader.update(cx, |reader, cx| {
            choose_sleep(reader, SleepChoice::EndOfChapter, cx);
            assert_eq!(reader.tts.sleep.and_then(SleepPlan::chapter), Some(0));
            choose_sleep(reader, SleepChoice::Minutes(5), cx);
            assert!(reader.tts.sleep_task.is_some() && reader.tts.sleep.and_then(SleepPlan::deadline).is_some());
            choose_sleep(reader, SleepChoice::Off, cx);
            assert!(reader.tts.sleep.is_none() && reader.tts.sleep_task.is_none());
            reader.sleep_tts(cx);
            assert!(reader.tts.playing && reader.tts.paused, "sleep must pause, keeping the position available to resume");
            assert_eq!(reader.tts.current, Some(4));
            reader.tts.close();
            assert!(!reader.tts.playing);
        });
    }

    #[test]
    fn unsupported_language_cannot_start_the_speech_worker() {
        let mut narration = Narration::for_languages(&["sv".into()]);
        assert!(narration.start(vec![SpeechPassage::new("A passage", true)], 0).is_err());
        assert!(narration.service.is_none());
        assert!(!narration.playing);
        assert!(Narration::for_languages(&[]).unavailable_reason.is_none());
        assert!(Narration::for_languages(&["en-GB".into()]).unavailable_reason.is_none());
    }

    #[test]
    fn stale_playback_events_cannot_advance_a_new_page_or_a_stopped_session() {
        let mut narration = Narration { generation: 5, playing: true, ..Default::default() };
        assert!(!narration.accepts(&SpeechEvent::Started { generation: 4, index: 0, glyph_ranges: vec![0..1] }));
        assert!(!narration.accepts(&SpeechEvent::Finished { generation: 4 }));
        assert!(narration.accepts(&SpeechEvent::Started { generation: 5, index: 0, glyph_ranges: vec![0..1] }));
        narration.pause();
        narration.navigated();
        assert!(narration.paused && narration.needs_restart, "navigation must not resume a paused session");
        narration.stop();
        assert!(!narration.accepts(&SpeechEvent::Started { generation: 5, index: 0, glyph_ranges: vec![0..1] }));
    }
}
