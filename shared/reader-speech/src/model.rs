use sherpa_onnx::{OfflineTts, OfflineTtsConfig, OfflineTtsKokoroModelConfig, OfflineTtsModelConfig, OfflineTtsVitsModelConfig};
use std::env;
#[cfg(any(target_os = "android", target_os = "linux"))]
use std::fs;
#[cfg(target_os = "linux")]
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
#[cfg(target_os = "linux")]
use tempfile::NamedTempFile;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Voice {
    #[default]
    Alba,
    George,
}

impl Voice {
    pub fn directory_name(self) -> &'static str {
        match self {
            Self::Alba => "alba",
            Self::George => "george",
        }
    }

    pub fn speaker_id(self) -> i32 {
        match self {
            Self::Alba => 0,
            Self::George => 26,
        }
    }

    fn model_file(self) -> &'static str {
        match self {
            Self::Alba => "en_GB-alba-medium.onnx",
            Self::George => "model.onnx",
        }
    }
}

type ModelPreparer = dyn Fn(Voice) -> Result<PathBuf, String> + Send + Sync;
static MODEL_PREPARER: OnceLock<Box<ModelPreparer>> = OnceLock::new();

/// Resolve platform assets on the synthesis worker when narration starts.
/// Android waits for its background extraction here without blocking the UI.
pub fn set_model_preparer(prepare: impl Fn(Voice) -> Result<PathBuf, String> + Send + Sync + 'static) {
    let _ = MODEL_PREPARER.set(Box::new(prepare));
}

pub fn model_directory() -> PathBuf {
    voice_directory(Voice::Alba)
}

pub fn voice_directory(voice: Voice) -> PathBuf {
    if voice == Voice::George
        && let Some(path) = env::var_os("BOKHEIM_TTS_GEORGE_MODEL_DIR")
    {
        return path.into();
    }
    if let Some(path) = env::var_os("BOKHEIM_TTS_MODEL_DIR") {
        let path = PathBuf::from(path);
        return if voice == Voice::Alba { path } else { path.parent().unwrap_or(Path::new(".")).join(voice.directory_name()) };
    }
    if let Ok(executable) = env::current_exe() {
        let packaged = executable.parent().unwrap_or_else(|| Path::new(".")).join("tts").join(voice.directory_name());
        if packaged.join(voice.model_file()).is_file() {
            return packaged;
        }
    }
    let data =
        env::var_os("XDG_DATA_HOME").map(PathBuf::from).or_else(|| env::var_os("LOCALAPPDATA").map(PathBuf::from)).or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))).unwrap_or_else(|| PathBuf::from("."));
    data.join("bokheim/tts").join(voice.directory_name())
}

/// Load Alba with its default inference thread count.
pub fn load_model() -> Result<OfflineTts, String> {
    load_voice(Voice::Alba)
}

/// Load the installed voice with an explicit synthesis thread count for profiling.
pub fn load_model_with_threads(threads: i32) -> Result<OfflineTts, String> {
    load_voice_with_threads(Voice::Alba, threads)
}

pub fn load_voice(voice: Voice) -> Result<OfflineTts, String> {
    // George needs more parallel synthesis work; Android's scheduler chooses
    // the CPU cores. Keep Alba's lighter inference at two threads.
    let threads = match voice {
        Voice::Alba => 2,
        Voice::George => 4,
    };
    load_voice_with_threads(voice, threads)
}

pub fn load_voice_with_threads(voice: Voice, threads: i32) -> Result<OfflineTts, String> {
    if threads < 1 {
        return Err("Speech thread count must be positive".into());
    }
    let root = match MODEL_PREPARER.get() {
        Some(prepare) => prepare(voice)?,
        None => voice_directory(voice),
    };
    let model = root.join(voice.model_file());
    let tokens = root.join("tokens.txt");
    let data_dir = root.join("espeak-ng-data");
    for path in [&model, &tokens, &data_dir] {
        if !path.exists() {
            return Err("The offline speech model is not installed.".into());
        }
    }
    // Android loads the packaged inference libraries here on the synthesis
    // worker, instead of linking them into the application's startup graph.
    #[cfg(target_os = "android")]
    sherpa_onnx_sys::initialize_tts()?;
    #[cfg(not(any(target_os = "android", target_os = "linux")))]
    let provider = "cpu".to_owned();
    #[cfg(target_os = "linux")]
    let runtime_options = {
        // Packaged voices can be read-only (AppImage/Flatpak). Keep these tiny
        // options in the user cache, and alive until session creation returns.
        let cache = env::var_os("XDG_CACHE_HOME").map(PathBuf::from).or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache"))).ok_or("Could not locate the speech runtime cache.")?.join("bokheim/tts");
        fs::create_dir_all(&cache).map_err(|error| format!("Could not create the speech runtime cache: {error}"))?;
        let mut options = NamedTempFile::new_in(cache).map_err(|error| format!("Could not configure offline speech memory: {error}"))?;
        options.write_all(b"EnableCpuMemArena=0\n").map_err(|error| format!("Could not configure offline speech memory: {error}"))?;
        options
    };
    #[cfg(target_os = "linux")]
    let provider = format!("cpu:{}", runtime_options.path().display());
    #[cfg(target_os = "android")]
    let provider = {
        // ORT's CPU arena retains larger synthesis workspaces until the model
        // is destroyed. Normal allocations return these between chunks rather
        // than allowing a long Android narration session to keep growing.
        let options = root.join("onnxruntime-cpu.conf");
        fs::write(&options, "EnableCpuMemArena=0\n").map_err(|error| format!("Could not configure offline speech memory: {error}"))?;
        format!("cpu:{}", options.display())
    };
    let mut model_config = OfflineTtsModelConfig { num_threads: threads, provider: Some(provider), ..Default::default() };
    match voice {
        Voice::Alba => {
            model_config.vits = OfflineTtsVitsModelConfig {
                model: Some(model.to_string_lossy().into_owned()),
                tokens: Some(tokens.to_string_lossy().into_owned()),
                data_dir: Some(data_dir.to_string_lossy().into_owned()),
                noise_scale: 0.75,
                ..Default::default()
            }
        }
        Voice::George => {
            let voices = root.join("voices.bin");
            let lexicon = root.join("lexicon-gb-en.txt");
            if !voices.is_file() || !lexicon.is_file() {
                return Err("The offline George speech assets are not installed.".into());
            }
            model_config.kokoro = OfflineTtsKokoroModelConfig {
                model: Some(model.to_string_lossy().into_owned()),
                voices: Some(voices.to_string_lossy().into_owned()),
                tokens: Some(tokens.to_string_lossy().into_owned()),
                data_dir: Some(data_dir.to_string_lossy().into_owned()),
                lexicon: Some(lexicon.to_string_lossy().into_owned()),
                lang: Some("en".into()),
                ..Default::default()
            };
        }
    }
    // Deliver completed sentence segments through the synthesis callback,
    // instead of waiting for all sentences in a reader chunk (notably for Alba).
    let config = OfflineTtsConfig { model: model_config, max_num_sentences: 1, ..Default::default() };
    OfflineTts::create(&config).ok_or_else(|| "Could not load the speech model.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sherpa_onnx::GenerationConfig;

    #[test]
    #[ignore = "requires the prepared bundled voice assets"]
    fn bundled_voices_synthesize_with_their_correct_speaker_ids() {
        for voice in [Voice::Alba, Voice::George, Voice::Alba] {
            let model = load_voice(voice).expect("load bundled voice");
            let config = GenerationConfig { sid: voice.speaker_id(), ..Default::default() };
            let audio = model.generate_with_config("The next chapter begins here.", &config, None::<fn(&[f32], f32) -> bool>).expect("synthesize voice");
            assert!(!audio.samples().is_empty());
            assert!(audio.samples().iter().all(|sample| sample.is_finite()));
            assert!(audio.samples().iter().any(|sample| sample.abs() > 1e-5));
            assert_eq!(audio.sample_rate(), if voice == Voice::George { 24000 } else { 22050 });
        }
    }
}
