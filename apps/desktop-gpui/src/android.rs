use app::host::defer_library_maintenance;
use app::host::startup_profile::StartupProfile;
use app::{AppDataLocation, host::BackendLaunch};
use gpui::Platform;
use gpui_android::{AndroidApp, AndroidPlatform, AndroidPlatformOptions};
use jni::EnvUnowned;
use jni::objects::{JClass, JObject, JString};
use jni::refs::Reference;
use jni::sys::{jboolean, jint, jlong, jstring};
use std::fs::File;
use std::os::fd::FromRawFd as _;
use std::path::Path;
use std::rc::Rc;
use std::sync::Once;
use std::sync::mpsc::TrySendError;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use crate::{IncomingBook, IncomingBookResult, UiLaunchOptions, open_ui};

static INCOMING_FILE_SENDER: OnceLock<async_channel::Sender<IncomingBookResult>> = OnceLock::new();
// Queue entries and storage snapshots remain valid if a callback unwinds.
// Recover these locks so a later JNI callback cannot panic on poisoning.
static EARLY_INCOMING_FILES: Mutex<Vec<IncomingBookResult>> = Mutex::new(Vec::new());
static NATIVE_BACK: OnceLock<async_channel::Sender<()>> = OnceLock::new();
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
        let mut state = STORAGE_ACCESS.lock().unwrap_or_else(PoisonError::into_inner);
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
                Ok::<_, app::BackendError>(())
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
    gpui_android::init_logging();
    if let Err(error) = run_android(app.clone()) {
        log::error!("Could not start Bokheim: {error}");
        finish_activity(&app);
    }
}

fn run_android(app: AndroidApp) -> Result<(), String> {
    let data_dir = app.internal_data_path().ok_or("Android did not provide a private application data directory")?;
    let (back_sender, back_receiver) = async_channel::unbounded();
    NATIVE_BACK.set(back_sender).ok();
    let (incoming_sender, incoming_files) = async_channel::bounded(8);
    INCOMING_FILE_SENDER.set(incoming_sender.clone()).ok();
    for file in std::mem::take(&mut *EARLY_INCOMING_FILES.lock().unwrap_or_else(PoisonError::into_inner)) {
        incoming_sender.try_send(file).ok();
    }
    // Retain the application context and class, never an Activity: media
    // commands remain valid after the activity is destroyed or recreated.
    let audio_vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr().cast()) };
    let (audio_class, audio_context, profiling, defer_background, defer_maintenance, load_system_fonts, cache_gpu_pipelines, force_opengl) = audio_vm
        .attach_current_thread(|env| {
            let raw_activity = app.activity_as_ptr() as jni::sys::jobject;
            let activity = unsafe { env.as_cast_raw::<jni::refs::Global<JObject>>(&raw_activity)? };
            let class = env.get_object_class(activity.as_ref())?;
            let class = env.new_global_ref(class)?;
            let context = env.call_method(activity.as_ref(), jni::jni_str!("getApplicationContext"), jni::jni_sig!("()Landroid/content/Context;"), &[])?.l()?;
            let intent = env.call_method(activity.as_ref(), jni::jni_str!("getIntent"), jni::jni_sig!("()Landroid/content/Intent;"), &[])?.l()?;
            let key = JString::from_str(env, "bokheim.input_profile")?;
            if env.call_method(&intent, jni::jni_str!("getBooleanExtra"), jni::jni_sig!("(Ljava/lang/String;Z)Z"), &[jni::objects::JValue::Object(key.as_ref()), jni::objects::JValue::Bool(false)])?.z()? {
                gpui::profiler::input_frame::enable();
            }
            let key = JString::from_str(env, "bokheim.startup_profile")?;
            let profiling = env.call_method(&intent, jni::jni_str!("getBooleanExtra"), jni::jni_sig!("(Ljava/lang/String;Z)Z"), &[jni::objects::JValue::Object(key.as_ref()), jni::objects::JValue::Bool(false)])?.z()?;
            let key = JString::from_str(env, "bokheim.startup_defer_background")?;
            let defer_background = profiling && env.call_method(&intent, jni::jni_str!("getBooleanExtra"), jni::jni_sig!("(Ljava/lang/String;Z)Z"), &[jni::objects::JValue::Object(key.as_ref()), jni::objects::JValue::Bool(false)])?.z()?;
            let key = JString::from_str(env, "bokheim.startup_defer_maintenance")?;
            let defer_maintenance = env.call_method(&intent, jni::jni_str!("getBooleanExtra"), jni::jni_sig!("(Ljava/lang/String;Z)Z"), &[jni::objects::JValue::Object(key.as_ref()), jni::objects::JValue::Bool(true)])?.z()?;
            let key = JString::from_str(env, "bokheim.android_system_fonts")?;
            let load_system_fonts = env.call_method(&intent, jni::jni_str!("getBooleanExtra"), jni::jni_sig!("(Ljava/lang/String;Z)Z"), &[jni::objects::JValue::Object(key.as_ref()), jni::objects::JValue::Bool(true)])?.z()?;
            let key = JString::from_str(env, "bokheim.gpu_pipeline_cache")?;
            let cache_gpu_pipelines = env.call_method(&intent, jni::jni_str!("getBooleanExtra"), jni::jni_sig!("(Ljava/lang/String;Z)Z"), &[jni::objects::JValue::Object(key.as_ref()), jni::objects::JValue::Bool(true)])?.z()?;
            let key = JString::from_str(env, "bokheim.force_opengl")?;
            let force_opengl = env.call_method(&intent, jni::jni_str!("getBooleanExtra"), jni::jni_sig!("(Ljava/lang/String;Z)Z"), &[jni::objects::JValue::Object(key.as_ref()), jni::objects::JValue::Bool(false)])?.z()?;
            Ok::<_, jni::errors::Error>((class, env.new_global_ref(context)?, profiling, defer_background, defer_maintenance, load_system_fonts, cache_gpu_pipelines, force_opengl))
        })
        .map_err(|error| format!("could not initialize Android audiobook bridge: {error}"))?;
    if profiling {
        StartupProfile::enable();
        gpui::profiler::startup::StartupProfile::enable();
    }
    let mut profile = StartupProfile::new("android_launch");
    let google_drive_available = audio_vm.attach_current_thread(|env| {
        env.call_static_method(&audio_class, jni::jni_str!("googleDriveAvailable"), jni::jni_sig!("(Landroid/content/Context;)Z"), &[jni::objects::JValue::Object(audio_context.as_ref())])?.z()
    }).unwrap_or_else(|error| {
        log::warn!("Google Drive availability could not be checked: {error}");
        false
    });
    app::host::set_google_play_services_available(google_drive_available);
    #[cfg(feature = "google-drive")]
    {
        let drive_vm = audio_vm.clone();
        let drive_class = audio_vm.attach_current_thread(|env| env.new_global_ref(&audio_class)).map_err(|_| "could not retain Android Google authorization class")?;
        app::host::android_authorization::configure(Arc::new(move |request| {
            drive_vm.attach_current_thread(|env| {
                let email = env.new_string(request.email.as_deref().unwrap_or_default())?;
                let rejected = env.new_string(request.rejected_token.as_deref().unwrap_or_default())?;
                env.call_static_method(&drive_class, jni::jni_str!("googleDriveAuthorize"), jni::jni_sig!("(JLjava/lang/String;ZLjava/lang/String;)V"), &[
                    jni::objects::JValue::Long(request.id as jlong), jni::objects::JValue::Object(email.as_ref()),
                    jni::objects::JValue::Bool(request.interactive), jni::objects::JValue::Object(rejected.as_ref()),
                ])?;
                Ok::<_, jni::errors::Error>(())
            }).map_err(|_| app::host::CloudAuthorizationError::AuthenticationRequired)
        }));
    }
    let taxonomy = audio_vm
        .attach_current_thread(|env| {
            let path = env.call_static_method(&audio_class, jni::jni_str!("taxonomyFile"), jni::jni_sig!("(Landroid/content/Context;)Ljava/lang/String;"), &[jni::objects::JValue::Object(audio_context.as_ref())])?.l()?;
            env.cast_local::<JString>(path)?.try_to_string(env)
        })
        .map_err(|error| format!("could not extract bundled taxonomy: {error}"))?;
    subject_projection::bundled::set_path(Path::new(&taxonomy)).map_err(|error| format!("could not configure bundled taxonomy: {error}"))?;
    profile.mark("taxonomy_prepare");
    let speech_vm = audio_vm.clone();
    let (speech_class, speech_context) =
        audio_vm.attach_current_thread(|env| Ok::<_, jni::errors::Error>((env.new_global_ref(&audio_class)?, env.new_global_ref(&audio_context)?))).map_err(|error| format!("could not retain Android speech context: {error}"))?;
    let speech_refs = Arc::new((speech_class, speech_context));
    let model_refs = speech_refs.clone();
    let model_vm = speech_vm.clone();
    reader_speech::set_model_preparer(move |voice| {
        model_vm
            .attach_current_thread(|env| {
                let voice = JString::from_str(env, voice.directory_name())?;
                let directory = env
                    .call_static_method(
                        &model_refs.0,
                        jni::jni_str!("speechModelDirectory"),
                        jni::jni_sig!("(Landroid/content/Context;Ljava/lang/String;)Ljava/lang/String;"),
                        &[jni::objects::JValue::Object(model_refs.1.as_ref()), jni::objects::JValue::Object(voice.as_ref())],
                    )?
                    .l()?;
                env.cast_local::<JString>(directory)?.try_to_string(env)
            })
            .map(Into::into)
            .map_err(|error| error.to_string())
    });
    reader_speech::android::initialize(move |command| {
        speech_vm
            .attach_current_thread(|env| {
                let command = JString::from_str(env, command)?;
                env.call_static_method(
                    &speech_refs.0,
                    jni::jni_str!("speechCommand"),
                    jni::jni_sig!("(Landroid/content/Context;Ljava/lang/String;)V"),
                    &[jni::objects::JValue::Object(speech_refs.1.as_ref()), jni::objects::JValue::Object(command.as_ref())],
                )?;
                Ok::<_, jni::errors::Error>(())
            })
            .map_err(|error| error.to_string())
    });
    audiobook_player::android::initialize(data_dir.join("audio-playback"), move |command| {
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
        .map_err(|error| format!("could not initialize Android account bridge: {error}"))?;
    browser_ui::android_authentication::initialize(move |command| {
        auth_vm
            .attach_current_thread(|env| {
                let command = JString::from_str(env, command)?;
                env.call_static_method(&auth_class, jni::jni_str!("authenticationCommand"), jni::jni_sig!("(Ljava/lang/String;)V"), &[jni::objects::JValue::Object(command.as_ref())])?;
                Ok::<_, jni::errors::Error>(())
            })
            .map_err(|error| error.to_string())
    });
    profile.mark("media_and_auth_bridges");
    let browser_vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr().cast()) };
    let browser_class = browser_vm
        .attach_current_thread(|env| {
            let raw_activity = app.activity_as_ptr() as jni::sys::jobject;
            let activity = unsafe { env.as_cast_raw::<jni::refs::Global<JObject>>(&raw_activity)? };
            let context = env.call_method(activity.as_ref(), jni::jni_str!("getApplicationContext"), jni::jni_sig!("()Landroid/content/Context;"), &[])?.l()?;
            let class = env.get_object_class(activity.as_ref())?;
            env.call_static_method(&class, jni::jni_str!("startNetworkCostMonitor"), jni::jni_sig!("(Landroid/content/Context;)V"), &[jni::objects::JValue::Object(context.as_ref())])?;
            rustls_platform_verifier::android::init_with_env(env, context)?;
            env.new_global_ref(class)
        })
        .map_err(|error| format!("could not initialize Android book browser bridge: {error}"))?;
    crate::book_browser::initialize(
        move |command| {
            browser_vm
                .attach_current_thread(|env| {
                    let command = JString::from_str(env, command)?;
                    env.call_static_method(&browser_class, jni::jni_str!("bookBrowserCommand"), jni::jni_sig!("(Ljava/lang/String;)V"), &[jni::objects::JValue::Object(command.as_ref())])?;
                    Ok::<_, jni::errors::Error>(())
                })
                .map_err(|error| error.to_string())
        },
        app::host::android_browser::receive,
    );
    app::host::android_browser::initialize(crate::book_browser::command);
    profile.mark("browser_and_network");
    gpui_platform::android_init(app.clone());
    profile.mark("platform_init");
    static PANIC_LOGGING: Once = Once::new();
    PANIC_LOGGING.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            log::error!("Android panic: {info}\n{}", std::backtrace::Backtrace::force_capture());
            previous(info);
        }));
    });
    log::info!("Bokheim Android entry point started");

    app::host::configure_opds_browser(data_dir.join("opds-webview"));
    let update_vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr().cast()) };
    let (update_class, update_context, play_store, self_updates_enabled) = update_vm
        .attach_current_thread(|env| {
            let raw_activity = app.activity_as_ptr() as jni::sys::jobject;
            let activity = unsafe { env.as_cast_raw::<jni::refs::Global<JObject>>(&raw_activity)? };
            let class = env.get_object_class(activity.as_ref())?;
            let class = env.new_global_ref(class)?;
            let play_store = env.call_static_method(&class, jni::jni_str!("isPlayStoreBuild"), jni::jni_sig!("()Z"), &[])?.z()?;
            let self_updates_enabled = env.call_static_method(&class, jni::jni_str!("isSelfUpdateEnabled"), jni::jni_sig!("()Z"), &[])?.z()?;
            let context = env.call_method(activity.as_ref(), jni::jni_str!("getApplicationContext"), jni::jni_sig!("()Landroid/content/Context;"), &[])?.l()?;
            Ok::<_, jni::errors::Error>((class, env.new_global_ref(context)?, play_store, self_updates_enabled))
        })
        .map_err(|error| format!("could not initialize Android update bridge: {error}"))?;
    let update_trial = if !self_updates_enabled {
        // No update host means no feed requests, APK staging or update controls.
        false
    } else {
        crate::android_updates::initialize(&data_dir, move |command| {
            update_vm
                .attach_current_thread(|env| {
                    let command = JString::from_str(env, command)?;
                    let result = env.call_static_method(
                        &update_class,
                        jni::jni_str!("updateCommand"),
                        jni::jni_sig!("(Landroid/content/Context;Ljava/lang/String;)Ljava/lang/String;"),
                        &[jni::objects::JValue::Object(update_context.as_ref()), jni::objects::JValue::Object(command.as_ref())],
                    );
                    if env.exception_check() {
                        env.exception_describe();
                        env.exception_clear();
                    }
                    let value = result?.l()?;
                    env.cast_local::<JString>(value)?.try_to_string(env)
                })
                .map_err(|e| e.to_string())
        })
        .map_err(|error| format!("failed to recover Android update: {error}"))?
    };
    profile.mark("update_recovery");
    let mut startup_permit = (update_trial || defer_background).then(app::host::defer_background_work);
    // Keep scan/sync/import I/O from competing with opening the remaining
    // libraries and rendering the first frame. Foreground commands stay live.
    let maintenance_permit = defer_maintenance.then(defer_library_maintenance);
    let incoming_directory = data_dir.join("incoming");
    std::fs::create_dir_all(&incoming_directory).map_err(|error| format!("failed to create Android incoming book directory: {error}"))?;
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
                        deliver_incoming_file(Err(app::BackendError::from(error).context(request.name)));
                    }
                }
            }
        })
        .map_err(|error| format!("failed to start incoming book worker: {error}"))?;
    log::info!("initializing backend in {}", data_dir.display());
    let backend_context = BackendLaunch::initialize(AppDataLocation::native_path(data_dir)).map_err(|error| format!("failed to initialize Android app-data directory: {error}"))?;
    let backend_context = if play_store { backend_context.with_private_library_storage() } else { backend_context };
    profile.mark("staging_and_launch_config");
    log::info!("backend context initialized");

    log::info!("constructing GPUI application");
    // Start the UI with bundled fonts. Device font I/O and registration run
    // in the background after the application window has been installed.
    let platform =
        AndroidPlatform::new_with_options(false, AndroidPlatformOptions { load_system_fonts: false, system_font_fallback: "Raleway".into(), cache_gpu_pipelines, graphics_backends: force_opengl.then_some(gpui_android::wgpu::Backends::GL) });
    let font_text_system = load_system_fonts.then(|| platform.text_system());
    let application = gpui::Application::with_platform(Rc::new(platform)).with_assets(ui_components::Assets);
    profile.mark("application_construct");
    log::info!("running GPUI application");
    application.run(move |cx| {
        cx.set_global(ui_components::SystemFontFamilies(if load_system_fonts { Vec::new() } else { vec!["NV Garamond".into(), "Raleway".into(), "Bodoni Moda".into()] }));
        let pending_fonts = font_text_system.map(|text_system| {
            let (ready, receiver) = async_channel::bounded::<()>(1);
            cx.set_global(reader_ui::SystemFontsLoading(receiver));
            (text_system, ready)
        });
        cx.set_global(browser_ui::AndroidStoragePolicy { all_files_access_available: !play_store });
        profile.mark("event_loop_ready");
        log::info!("opening shared Bokheim UI");
        let opened = open_ui(backend_context, UiLaunchOptions::MOBILE, Some(incoming_files), cx);
        if opened.is_none() {
            log::error!("failed to open the Android application UI");
            finish_activity(&app);
            cx.quit();
            return;
        }
        if let Some((text_system, ready)) = pending_fonts {
            let fonts = cx.background_executor().spawn(async move {
                crate::android_fonts::load(text_system.as_ref());
                text_system.all_font_names()
            });
            cx.spawn(async move |cx| {
                let families = fonts.await;
                let _ = cx.update(|cx| {
                    cx.set_global(ui_components::SystemFontFamilies(families));
                    cx.refresh_windows();
                });
                // No messages are sent: closing wakes every waiting reader.
                drop(ready);
            })
            .detach();
        }
        if let Some(root) = opened.clone() {
            let back_app = app.clone();
            cx.spawn(async move |cx| {
                while back_receiver.recv().await.is_ok() {
                    // This is navigation, not a synthetic keyboard event.
                    if matches!(root.update_in(cx, |root, window, cx| root.native_back(window, cx)), Ok(false)) {
                        finish_activity(&back_app);
                    }
                }
            })
            .detach();
        }
        profile.mark("ui_open");
        if opened.is_some() {
            if let Err(error) = crate::android_updates::healthy() {
                log::error!("failed to commit Android update startup: {error}");
                finish_activity(&app);
                cx.quit();
                return;
            }
            if !defer_background {
                if let Some(permit) = startup_permit.take() {
                    permit.release();
                }
            }
        }
        log::info!("shared Bokheim UI opened");
        if opened.is_some() {
            profile.mark("update_healthy");
            let Some(window) = cx.windows().first().copied() else {
                log::error!("Android startup window closed before its first frame");
                finish_activity(&app);
                cx.quit();
                return;
            };
            let _ = window.update(cx, |_, window, _| {
                window.on_next_frame(move |_, _| {
                    profile.mark("frame_callback");
                    if let Some(permit) = maintenance_permit {
                        log::info!("Android library maintenance released after first frame callback");
                        drop(permit);
                    }
                    if let Some(permit) = startup_permit {
                        permit.release();
                    }
                })
            });
        }
    });
    Ok(())
}

fn finish_activity(app: &AndroidApp) {
    let activity_app = app.clone();
    app.run_on_java_main_thread(Box::new(move || {
        let vm = unsafe { jni::JavaVM::from_raw(activity_app.vm_as_ptr().cast()) };
        let result = vm.attach_current_thread(|env| {
            let raw = activity_app.activity_as_ptr() as jni::sys::jobject;
            let activity = unsafe { env.as_cast_raw::<jni::refs::Global<JObject>>(&raw)? };
            env.call_method(activity.as_ref(), jni::jni_str!("finish"), jni::jni_sig!("()V"), &[])?;
            Ok::<_, jni::errors::Error>(())
        });
        if let Err(error) = result {
            log::error!("Could not finish Android activity: {error}");
        }
    }));
}

#[unsafe(no_mangle)]
extern "system" fn Java_se_bokheim_reader_gpui_MainActivity_nativeBackPressed(_env: EnvUnowned<'_>, _activity: JObject<'_>) {
    if let Some(sender) = NATIVE_BACK.get() {
        let _ = sender.try_send(());
    }
}

#[unsafe(no_mangle)]
extern "system" fn Java_se_bokheim_reader_gpui_MainActivity_nativeNetworkCostChanged(_env: EnvUnowned<'_>, _class: JClass<'_>, cost: jint) {
    use app::host::transfers::{NetworkCost, POLICY};
    POLICY.set_network_cost(match cost {
        1 => NetworkCost::Unmetered,
        2 => NetworkCost::Metered,
        _ => NetworkCost::Unknown,
    });
}

#[unsafe(no_mangle)]
extern "system" fn Java_se_bokheim_reader_gpui_MainActivity_nativeGooglePlayServicesAvailable(_env: EnvUnowned<'_>, _class: JClass<'_>, available: jboolean) {
    app::host::set_google_play_services_available(available);
}

// Keep the Java callback ABI available in builds without provider support.
#[cfg(not(feature = "google-drive"))]
#[unsafe(no_mangle)]
extern "system" fn Java_se_bokheim_reader_gpui_MainActivity_nativeGoogleDriveAuthorization(_env: EnvUnowned<'_>, _class: JClass<'_>, _id: jlong, _token: JString<'_>, _error: jint) {}

#[cfg(feature = "google-drive")]
#[unsafe(no_mangle)]
extern "system" fn Java_se_bokheim_reader_gpui_MainActivity_nativeGoogleDriveAuthorization<'caller>(mut unowned_env: EnvUnowned<'caller>, _class: JClass<'caller>, id: jlong, token: JString<'caller>, error: jint) {
    use app::host::{AndroidGoogleAccessGrant, CloudAuthorizationError};
    let result = if error == 0 {
        unowned_env.with_env(|env| token.try_to_string(env).map(Some)).resolve::<jni::errors::LogErrorAndDefault>()
            .map(|access_token| AndroidGoogleAccessGrant { access_token, expires_in_seconds: 300 })
            .ok_or_else(|| CloudAuthorizationError::InvalidResponse("Google returned an invalid authorization grant".into()))
    } else {
        Err(match error {
            1 => CloudAuthorizationError::InvalidRequest("Google Drive connection was cancelled".into()),
            2 | 4 => CloudAuthorizationError::AuthenticationRequired,
            3 => CloudAuthorizationError::PermissionDenied,
            5 => CloudAuthorizationError::Transport { failure: app::Failure::Offline, message: "Could not connect to Google".into() },
            7 => CloudAuthorizationError::Retryable("Another Google authorization request is in progress".into()),
            _ => CloudAuthorizationError::Retryable("Google Drive could not be connected; try again".into()),
        })
    };
    if id > 0 { app::host::android_authorization::complete(id as u64, result); }
}

#[unsafe(no_mangle)]
extern "system" fn Java_se_bokheim_reader_gpui_UpdateMetadata_nativeJson<'caller>(mut unowned_env: EnvUnowned<'caller>, _class: jni::objects::JClass<'caller>) -> jni::sys::jstring {
    unowned_env
        .with_env(|env| -> jni::errors::Result<jni::sys::jstring> {
            match crate::update_metadata::json() {
                Ok(json) => Ok(env.new_string(json)?.into_raw()),
                Err(error) => {
                    log::error!("Could not read Android update metadata: {error}");
                    Ok(std::ptr::null_mut())
                }
            }
        })
        .resolve::<jni::errors::LogErrorAndDefault>()
}

#[unsafe(no_mangle)]
extern "system" fn Java_se_bokheim_reader_gpui_UpdateMigrationService_nativePrepare<'caller>(mut unowned_env: EnvUnowned<'caller>, _class: JObject<'caller>, job: JString<'caller>, taxonomy: JString<'caller>) {
    unowned_env
        .with_env(|env| -> jni::errors::Result<()> {
            let path = job.try_to_string(env)?;
            let taxonomy = taxonomy.try_to_string(env)?;
            if let Err(error) = subject_projection::bundled::set_path(Path::new(&taxonomy)) {
                log::error!("Android bundled taxonomy failed: {error}");
                return Ok(());
            }
            if let Err(error) = linux_update_host::prepare_candidate(Path::new(&path), env!("CARGO_PKG_VERSION")) {
                // No result file is produced on failure. The parent waits for helper
                // process death before reading it or touching a live database.
                log::error!("Android update candidate failed: {error}");
            }
            Ok(())
        })
        .resolve::<jni::errors::LogErrorAndDefault>();
}

fn deliver_incoming_file(file: IncomingBookResult) {
    if let Some(sender) = INCOMING_FILE_SENDER.get() {
        if let Err(error) = sender.try_send(file) {
            if let Ok(file) = error.into_inner() {
                let _ = std::fs::remove_file(file.path);
            }
        }
    } else {
        EARLY_INCOMING_FILES.lock().unwrap_or_else(PoisonError::into_inner).push(file);
    }
}

fn cleanup_abandoned_incoming_files(directory: &Path) {
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
                        TrySendError::Full(request) | TrySendError::Disconnected(request) => request,
                    };
                    deliver_incoming_file(Err(app::BackendError::message(format!("could not queue {}: incoming book queue is full or stopped", request.name))));
                }
                None => deliver_incoming_file(Err(app::BackendError::message("incoming book worker is not initialized"))),
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
            let mut state = STORAGE_ACCESS.lock().unwrap_or_else(PoisonError::into_inner);
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
extern "system" fn Java_se_bokheim_reader_gpui_SpeechService_nativeState<'caller>(mut unowned_env: EnvUnowned<'caller>, _class: JClass<'caller>, id: jlong) -> jstring {
    unowned_env
        .with_env(|env| -> jni::errors::Result<_> {
            let state = reader_speech::android::snapshot(id as u64).unwrap_or_default();
            Ok(JString::from_str(env, state)?.into_raw())
        })
        .resolve::<jni::errors::LogErrorAndDefault>()
}

#[unsafe(no_mangle)]
extern "system" fn Java_se_bokheim_reader_gpui_SpeechService_nativeControl<'caller>(mut unowned_env: EnvUnowned<'caller>, _class: JClass<'caller>, id: jlong, command: JString<'caller>) -> jboolean {
    unowned_env
        .with_env(|env| -> jni::errors::Result<_> {
            let command = command.try_to_string(env)?;
            Ok(reader_speech::android::control(id as u64, &command) as jboolean)
        })
        .resolve::<jni::errors::LogErrorAndDefault>()
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

#[unsafe(no_mangle)]
extern "system" fn Java_se_bokheim_reader_gpui_MainActivity_nativeBookBrowserEvent<'caller>(mut unowned_env: EnvUnowned<'caller>, _class: jni::objects::JClass<'caller>, id: jlong, message: JString<'caller>) {
    unowned_env
        .with_env(|env| -> jni::errors::Result<()> {
            crate::book_browser::receive(id as u64, &message.try_to_string(env)?);
            Ok(())
        })
        .resolve::<jni::errors::LogErrorAndDefault>();
}
