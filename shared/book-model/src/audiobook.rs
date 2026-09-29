use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudiobookChapter {
    pub title: String,
    pub start: Duration,
    pub end: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ChapterOrigin {
    EmbeddedM4b,
    TrackFiles,
    CueSheet,
    Audnexus,
    FullAudiobookFallback,
}
