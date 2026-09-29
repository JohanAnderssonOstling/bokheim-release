use app::{AppDataLocation, host::BackendLaunch};
use gpui_android::AndroidApp;
use jni::EnvUnowned;
use jni::objects::{JObject, JString};
use jni::refs::Reference;
use jni::sys::{jboolean, jint, jlong};
use std::fs::File;
use std::os::fd::FromRawFd as _;
use std::sync::{Mutex, OnceLock};

use crate::{IncomingBook, IncomingBookResult, UiLaunchOptions, open_ui};

static INCOMING_FILE_SENDER: OnceLock<async_channel::Sender<IncomingBookResult>> = OnceLock::new();
static EARLY_INCOMING_FILES: Mutex<Vec<IncomingBookResult>> = Mutex::new(Vec::new());
static STAGING_QUEUE: OnceLock<std::sync::mpsc::SyncSender<StagingRequest>> = OnceLock::new();

#[derive(Clone)]
struct StorageAccess {
    granted: bool,
    documents_root: String,
}

static STORAGE_ACCESS: Mutex<(Option<async_channel::Sender<StorageAccess>>, Option<StorageAccess>)> = Mutex::new((None, None));

pub(crate) fn watch_storage_access(backend: app::AppClient, private_root: std::path::PathBuf, cx: &mut gpui::App) {
    let (sender, receiver) = async_channel::unbounded();
    {
        let mut state = STORAGE_ACCESS.lock().unwrap();
        if let Some(access) = state.1.clone() {
            sender.try_send(access).ok();
        }
        state.0 = Some(sender);
    }
    cx.spawn(async move |_| {
        let private_root = private_root.to_string_lossy().into_owned();
        while let Ok(access) = receiver.recv().await {
            let outcome = async {
                let current = backend.default_library_save_location().await?;
                // Only switch our automatic destinations. Never replace a
                // user's custom save location or move existing libraries.
                if current.as_deref().is_none_or(|current| same_storage_root(current, &private_root) || same_storage_root(current, &access.documents_root)) {
                    let root = if access.granted { access.documents_root } else { private_root.clone() };
                    if !current.as_deref().is_some_and(|current| same_storage_root(current, &root)) {
                        backend.set_default_library_save_location(root).await?;
                    }
                }
                Ok::<_, String>(())
            }
            .await;
            if let Err(error) = outcome {
                log::error!("Could not update Android library storage: {error}");
            }
        }
    })
    .detach();
}

fn same_storage_root(left: &str, right: &str) -> bool {
    left == right || std::fs::canonicalize(left).ok().zip(std::fs::canonicalize(right).ok()).is_some_and(|(left, right)| left == right)
}

struct StagingRequest {
    name: String,
    file: File,
}

#[unsafe(no_mangle)]
fn android_main(app: AndroidApp) {
    let (incoming_sender, incoming_files) = async_channel::bounded(8);
    INCOMING_FILE_SENDER.set(incoming_sender.clone()).ok();
    for file in std::mem::take(&mut *EARLY_INCOMING_FILES.lock().unwrap()) {
        incoming_sender.try_send(file).ok();
    }
    // Retain the application context and class, never an Activity: media
    // commands remain valid after the activity is destroyed or recreated.
    let audio_vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr().cast()) };
    let (audio_class, audio_context) = audio_vm
        .attach_current_thread(|env| {
            let raw_activity = app.activity_as_ptr() as jni::sys::jobject;
            let activity = unsafe { env.as_cast_raw::<jni::refs::Global<JObject>>(&raw_activity)? };
            let class = env.get_object_class(activity.as_ref())?;
            let class = env.new_global_ref(class)?;
            let context = env.call_method(activity.as_ref(), jni::jni_str!("getApplicationContext"), jni::jni_sig!("()Landroid/content/Context;"), &[])?.l()?;
            Ok::<_, jni::errors::Error>((class, env.new_global_ref(context)?))
        })
        .expect("could not initialize Android audiobook bridge");
    audiobook_player::android::initialize(app.internal_data_path().unwrap().join("audio-playback"), move |command| {
        audio_vm
            .attach_current_thread(|env| {
                let command = JString::from_str(env, command)?;
                env.call_static_method(
                    &audio_class,
                    jni::jni_str!("audiobookCommand"),
                    jni::jni_sig!("(Landroid/content/Context;Ljava/lang/String;)V"),
                    &[jni::objects::JValue::Object(audio_context.as_ref()), jni::objects::JValue::Object(command.as_ref())],
                )?;
                Ok::<_, jni::errors::Error>(())
            })
            .map_err(|error| error.to_string())
    });
    let auth_vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr().cast()) };
    let auth_class = auth_vm
        .attach_current_thread(|env| {
            let raw_activity = app.activity_as_ptr() as jni::sys::jobject;
            let activity = unsafe { env.as_cast_raw::<jni::refs::Global<JObject>>(&raw_activity)? };
            let class = env.get_object_class(activity.as_ref())?;
            env.new_global_ref(class)
        })
        .expect("could not initialize Android account bridge");
    browser_ui::android_authentication::initialize(move |command| {
        auth_vm
            .attach_current_thread(|env| {
                let command = JString::from_str(env, command)?;
                env.call_static_method(&auth_class, jni::jni_str!("authenticationCommand"), jni::jni_sig!("(Ljava/lang/String;)V"), &[jni::objects::JValue::Object(command.as_ref())])?;
                Ok::<_, jni::errors::Error>(())
            })
            .map_err(|error| error.to_string())
    });
    gpui_platform::android_init(app.clone());
    static PANIC_LOGGING: std::sync::Once = std::sync::Once::new();
    PANIC_LOGGING.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            log::error!("Android panic: {info}\n{}", std::backtrace::Backtrace::force_capture());
            previous(info);
        }));
    });
    log::info!("Bokheim Android entry point started");

    let data_dir = app.internal_data_path().expect("Android did not provide a private application data directory");
    let update_vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr().cast()) };
    let (update_class, update_context) = update_vm.attach_current_thread(|env| {
        let raw_activity = app.activity_as_ptr() as jni::sys::jobject;
        let activity = unsafe { env.as_cast_raw::<jni::refs::Global<JObject>>(&raw_activity)? };
        let class = env.get_object_class(activity.as_ref())?;
        let class = env.new_global_ref(class)?;
        let context = env.call_method(activity.as_ref(), jni::jni_str!("getApplicationContext"), jni::jni_sig!("()Landroid/content/Context;"), &[])?.l()?;
        Ok::<_, jni::errors::Error>((class, env.new_global_ref(context)?))
    }).expect("could not initialize Android update bridge");
    let update_trial = crate::android_updates::initialize(&data_dir, move |command| {
        update_vm.attach_current_thread(|env| {
            let command = JString::from_str(env, command)?;
            let result = env.call_static_method(&update_class, jni::jni_str!("updateCommand"),
                jni::jni_sig!("(Landroid/content/Context;Ljava/lang/String;)Ljava/lang/String;"),
                &[jni::objects::JValue::Object(update_context.as_ref()), jni::objects::JValue::Object(command.as_ref())]);
            if env.exception_check() {
                env.exception_describe();
                env.exception_clear();
            }
            let value = result?.l()?;
            env.cast_local::<JString>(value)?.try_to_string(env)
        }).map_err(|e| e.to_string())
    }).expect("failed to recover Android update");
    let startup_permit = update_trial.then(app::host::defer_background_work);
    let incoming_directory = data_dir.join("incoming");
    std::fs::create_dir_all(&incoming_directory).expect("failed to create Android incoming book directory");
    cleanup_abandoned_incoming_files(&incoming_directory);
    let (staging_sender, staging_receiver) = std::sync::mpsc::sync_channel::<StagingRequest>(2);
    STAGING_QUEUE.set(staging_sender).ok();
    std::thread::Builder::new()
        .name("incoming-book".to_owned())
        .spawn(move || {
            for request in staging_receiver {
                let path = incoming_directory.join(format!("{}.part", uuid::Uuid::new_v4()));
                match crate::stage_incoming_book(request.file, &path) {
                    Ok(()) => deliver_incoming_file(Ok(IncomingBook { name: request.name, path })),
                    Err(error) => {
                        let _ = std::fs::remove_file(path);
                        deliver_incoming_file(Err(format!("{}: {error}", request.name)));
                    }
                }
            }
        })
        .expect("failed to start incoming book worker");
    log::info!("initializing backend in {}", data_dir.display());
    let backend_context = BackendLaunch::initialize(AppDataLocation::native_path(data_dir)).expect("failed to initialize Android app-data directory");
    log::info!("backend context initialized");

    log::info!("constructing GPUI application");
    let application = gpui_platform::application().with_assets(ui_components::Assets);
    log::info!("running GPUI application");
    application.run(move |cx| {
        log::info!("opening shared Bokheim UI");
        let opened = open_ui(backend_context, UiLaunchOptions::MOBILE, Some(incoming_files), cx);
        if opened.is_some() {
            crate::android_updates::healthy().expect("failed to commit Android update startup");
            if let Some(permit) = startup_permit { permit.release(); }
        }
        log::info!("shared Bokheim UI opened");
    });
}

#[unsafe(no_mangle)]
extern "system" fn Java_se_bokheim_reader_gpui_UpdateMetadata_nativeJson<'caller>(mut unowned_env: EnvUnowned<'caller>, _class: jni::objects::JClass<'caller>) -> jni::sys::jstring {
    unowned_env.with_env(|env| -> jni::errors::Result<jni::sys::jstring> {
        match crate::update_metadata::json() {
            Ok(json) => Ok(env.new_string(json)?.into_raw()),
            Err(error) => {
                log::error!("Could not read Android update metadata: {error}");
                Ok(std::ptr::null_mut())
            }
        }
    }).resolve::<jni::errors::LogErrorAndDefault>()
}

#[unsafe(no_mangle)]
extern "system" fn Java_se_bokheim_reader_gpui_UpdateMigrationService_nativePrepare<'caller>(mut unowned_env: EnvUnowned<'caller>, _class: JObject<'caller>, job: JString<'caller>) {
    unowned_env.with_env(|env| -> jni::errors::Result<()> {
        let path = job.try_to_string(env)?;
        if let Err(error) = linux_update_host::prepare_candidate(std::path::Path::new(&path), env!("CARGO_PKG_VERSION")) {
            // No result file is produced on failure. The parent waits for helper
            // process death before reading it or touching a live database.
            log::error!("Android update candidate failed: {error}");
        }
        Ok(())
    }).resolve::<jni::errors::LogErrorAndDefault>();
}

fn deliver_incoming_file(file: IncomingBookResult) {
    if let Some(sender) = INCOMING_FILE_SENDER.get() {
        if let Err(error) = sender.try_send(file) {
            if let Ok(file) = error.into_inner() {
                let _ = std::fs::remove_file(file.path);
            }
        }
    } else {
        EARLY_INCOMING_FILES.lock().unwrap().push(file);
    }
}

fn cleanup_abandoned_incoming_files(directory: &std::path::Path) {
    let cutoff = std::time::SystemTime::now().checked_sub(std::time::Duration::from_secs(24 * 60 * 60));
    let Ok(entries) = std::fs::read_dir(directory) else { return };
    for entry in entries.flatten() {
        let stale = cutoff.is_some_and(|cutoff| entry.metadata().ok().and_then(|metadata| metadata.modified().ok()).is_some_and(|modified| modified <= cutoff));
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[unsafe(no_mangle)]
extern "system" fn Java_se_bokheim_reader_gpui_MainActivity_nativeOpenBook<'caller>(mut unowned_env: EnvUnowned<'caller>, _activity: JObject<'caller>, name: JString<'caller>, file_descriptor: jint) {
    unowned_env
        .with_env(|env| -> jni::errors::Result<()> {
            if file_descriptor < 0 {
                log::error!("Android returned an invalid incoming book descriptor");
                return Ok(());
            }
            // SAFETY: ParcelFileDescriptor.detachFd transfers ownership of
            // this valid descriptor from Java to the native caller.
            let file = unsafe { File::from_raw_fd(file_descriptor) };
            let name = name.try_to_string(env)?;
            let request = StagingRequest { name, file };
            match STAGING_QUEUE.get().map(|queue| queue.try_send(request)) {
                Some(Ok(())) => {}
                Some(Err(error)) => {
                    let request = match error {
                        std::sync::mpsc::TrySendError::Full(request) | std::sync::mpsc::TrySendError::Disconnected(request) => request,
                    };
                    deliver_incoming_file(Err(format!("could not queue {}: incoming book queue is full or stopped", request.name)));
                }
                None => deliver_incoming_file(Err("incoming book worker is not initialized".to_owned())),
            }
            Ok(())
        })
        .resolve::<jni::errors::LogErrorAndDefault>();
}

#[unsafe(no_mangle)]
extern "system" fn Java_se_bokheim_reader_gpui_MainActivity_nativeSelectedFileDescriptor<'caller>(mut unowned_env: EnvUnowned<'caller>, _activity: JObject<'caller>, request_id: jlong, name: JString<'caller>, file_descriptor: jint) {
    unowned_env
        .with_env(|env| -> jni::errors::Result<()> {
            if file_descriptor < 0 {
                gpui_android::complete_file_prompt(request_id as u64, Some(anyhow::anyhow!("Android returned an invalid file descriptor")), false);
                return Ok(());
            }
            // SAFETY: ParcelFileDescriptor.detachFd transfers ownership of
            // this valid descriptor from Java to the native caller.
            let file = unsafe { File::from_raw_fd(file_descriptor) };
            let name = name.try_to_string(env)?;
            gpui_android::selected_file_descriptor(request_id as u64, name, file);
            Ok(())
        })
        .resolve::<jni::errors::LogErrorAndDefault>();
}

#[unsafe(no_mangle)]
extern "system" fn Java_se_bokheim_reader_gpui_MainActivity_nativeFilePromptResult<'caller>(mut unowned_env: EnvUnowned<'caller>, _activity: JObject<'caller>, request_id: jlong, error: JString<'caller>, cancelled: jboolean) {
    unowned_env
        .with_env(|env| -> jni::errors::Result<()> {
            let error = if error.is_null() { None } else { Some(anyhow::anyhow!(error.try_to_string(env)?)) };
            gpui_android::complete_file_prompt(request_id as u64, error, cancelled);
            Ok(())
        })
        .resolve::<jni::errors::LogErrorAndDefault>();
}

#[unsafe(no_mangle)]
extern "system" fn Java_se_bokheim_reader_gpui_MainActivity_nativeSelectedDirectoryRoot<'caller>(mut unowned_env: EnvUnowned<'caller>, _activity: JObject<'caller>, request_id: jlong, name: JString<'caller>) {
    unowned_env
        .with_env(|env| -> jni::errors::Result<()> {
            gpui_android::selected_directory_root(request_id as u64, name.try_to_string(env)?);
            Ok(())
        })
        .resolve::<jni::errors::LogErrorAndDefault>();
}

#[unsafe(no_mangle)]
extern "system" fn Java_se_bokheim_reader_gpui_MainActivity_nativeSelectedDirectoryLocalRoot<'caller>(mut unowned_env: EnvUnowned<'caller>, _activity: JObject<'caller>, request_id: jlong, path: JString<'caller>) {
    unowned_env
        .with_env(|env| -> jni::errors::Result<()> {
            gpui_android::selected_directory_local_root(request_id as u64, path.try_to_string(env)?.into());
            Ok(())
        })
        .resolve::<jni::errors::LogErrorAndDefault>();
}

#[unsafe(no_mangle)]
extern "system" fn Java_se_bokheim_reader_gpui_MainActivity_nativeStorageAccessChanged<'caller>(mut unowned_env: EnvUnowned<'caller>, _activity: JObject<'caller>, granted: jboolean, documents_root: JString<'caller>) {
    unowned_env
        .with_env(|env| -> jni::errors::Result<()> {
            let access = StorageAccess { granted, documents_root: documents_root.try_to_string(env)? };
            let mut state = STORAGE_ACCESS.lock().unwrap();
            state.1 = Some(access.clone());
            if let Some(sender) = &state.0 {
                sender.try_send(access).ok();
            }
            Ok(())
        })
        .resolve::<jni::errors::LogErrorAndDefault>();
}

#[unsafe(no_mangle)]
extern "system" fn Java_se_bokheim_reader_gpui_MainActivity_nativeSelectedDirectoryPath<'caller>(mut unowned_env: EnvUnowned<'caller>, _activity: JObject<'caller>, request_id: jlong, path_json: JString<'caller>) {
    unowned_env
        .with_env(|env| -> jni::errors::Result<()> {
            match serde_json::from_str::<Vec<String>>(&path_json.try_to_string(env)?) {
                Ok(path) => gpui_android::selected_directory_path(request_id as u64, path),
                Err(error) => gpui_android::complete_directory_prompt(request_id as u64, Some(anyhow::anyhow!("invalid Android directory path: {error}")), false),
            }
            Ok(())
        })
        .resolve::<jni::errors::LogErrorAndDefault>();
}

#[unsafe(no_mangle)]
extern "system" fn Java_se_bokheim_reader_gpui_MainActivity_nativeSelectedDirectoryFile<'caller>(
    mut unowned_env: EnvUnowned<'caller>, _activity: JObject<'caller>, request_id: jlong, path_json: JString<'caller>, uri: JString<'caller>, size_bytes: jlong,
) {
    unowned_env
        .with_env(|env| -> jni::errors::Result<()> {
            match serde_json::from_str::<Vec<String>>(&path_json.try_to_string(env)?) {
                Ok(path) => gpui_android::selected_directory_file(request_id as u64, path, uri.try_to_string(env)?, u64::try_from(size_bytes).ok()),
                Err(error) => gpui_android::complete_directory_prompt(request_id as u64, Some(anyhow::anyhow!("invalid Android directory file path: {error}")), false),
            }
            Ok(())
        })
        .resolve::<jni::errors::LogErrorAndDefault>();
}

#[unsafe(no_mangle)]
extern "system" fn Java_se_bokheim_reader_gpui_MainActivity_nativeDirectoryPromptResult<'caller>(mut unowned_env: EnvUnowned<'caller>, _activity: JObject<'caller>, request_id: jlong, error: JString<'caller>, cancelled: jboolean) {
    unowned_env
        .with_env(|env| -> jni::errors::Result<()> {
            let error = if error.is_null() { None } else { Some(anyhow::anyhow!(error.try_to_string(env)?)) };
            gpui_android::complete_directory_prompt(request_id as u64, error, cancelled);
            Ok(())
        })
        .resolve::<jni::errors::LogErrorAndDefault>();
}

#[unsafe(no_mangle)]
extern "system" fn Java_se_bokheim_reader_gpui_MainActivity_nativeVolumeButton<'caller>(mut unowned_env: EnvUnowned<'caller>, _activity: JObject<'caller>, next: jboolean) {
    unowned_env
        .with_env(|_| -> jni::errors::Result<()> {
            gpui_android::volume_button_pressed(next);
            Ok(())
        })
        .resolve::<jni::errors::LogErrorAndDefault>();
}

#[unsafe(no_mangle)]
extern "system" fn Java_se_bokheim_reader_gpui_MainActivity_nativeWindowInsets<'caller>(
    mut unowned_env: EnvUnowned<'caller>, _activity: JObject<'caller>, safe_left: jint, safe_top: jint, safe_right: jint, safe_bottom: jint, ime_left: jint, ime_top: jint, ime_right: jint, ime_bottom: jint,
) {
    unowned_env
        .with_env(|_| -> jni::errors::Result<()> {
            gpui_android::window_insets_changed(safe_left, safe_top, safe_right, safe_bottom, ime_left, ime_top, ime_right, ime_bottom);
            Ok(())
        })
        .resolve::<jni::errors::LogErrorAndDefault>();
}

#[unsafe(no_mangle)]
extern "system" fn Java_se_bokheim_reader_gpui_MainActivity_nativeReaderChromeRevealed<'caller>(mut unowned_env: EnvUnowned<'caller>, _activity: JObject<'caller>) {
    unowned_env
        .with_env(|_| -> jni::errors::Result<()> {
            gpui_android::reader_chrome_revealed();
            Ok(())
        })
        .resolve::<jni::errors::LogErrorAndDefault>();
}

#[unsafe(no_mangle)]
extern "system" fn Java_se_bokheim_reader_gpui_AudiobookService_nativePlaybackState<'caller>(mut unowned_env: EnvUnowned<'caller>, _class: jni::objects::JClass<'caller>, state: JString<'caller>) {
    unowned_env
        .with_env(|env| -> jni::errors::Result<()> {
            audiobook_player::android::playback_state(&state.try_to_string(env)?);
            Ok(())
        })
        .resolve::<jni::errors::LogErrorAndDefault>();
}

#[unsafe(no_mangle)]
extern "system" fn Java_se_bokheim_reader_gpui_AudiobookService_nativeInterruptAudio(_env: EnvUnowned<'_>, _class: jni::objects::JClass<'_>, id: jni::sys::jlong) {
    if id > 0 {
        audiobook_player::android::interrupt_source(id as u64);
    }
}

#[unsafe(no_mangle)]
extern "system" fn Java_se_bokheim_reader_gpui_AudiobookService_nativeReadAudio<'caller>(
    mut unowned_env: EnvUnowned<'caller>, _class: jni::objects::JClass<'caller>, id: jni::sys::jlong, offset: jni::sys::jlong, length: jni::sys::jint,
) -> jni::sys::jbyteArray {
    unowned_env
        .with_env(|env| -> jni::errors::Result<jni::sys::jbyteArray> {
            if id <= 0 || offset < 0 || length < 0 {
                return Ok(std::ptr::null_mut());
            }
            match audiobook_player::android::read_source(id as u64, offset as u64, length as usize) {
                Ok(bytes) => Ok(env.byte_array_from_slice(&bytes)?.into_raw()),
                Err(error) => {
                    log::warn!("Could not read Android audiobook: {error}");
                    Ok(std::ptr::null_mut())
                }
            }
        })
        .resolve::<jni::errors::LogErrorAndDefault>()
}

#[unsafe(no_mangle)]
extern "system" fn Java_se_bokheim_reader_gpui_MainActivity_nativeAuthenticationRequest<'caller>(mut unowned_env: EnvUnowned<'caller>, _activity: JObject<'caller>, id: jlong, message: JString<'caller>) {
    unowned_env
        .with_env(|env| -> jni::errors::Result<()> {
            browser_ui::android_authentication::receive(id as u64, &message.try_to_string(env)?);
            Ok(())
        })
        .resolve::<jni::errors::LogErrorAndDefault>();
}
