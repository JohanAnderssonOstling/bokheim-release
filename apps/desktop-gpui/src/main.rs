use app::{AppDataLocation, host::BackendLaunch};

fn main() {
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    if desktop_gpui::update_entry::candidate_entry() {
        return;
    }
    if app::host::cpu::run_pdf_cover_process() {
        return;
    }
    app::host::cpu::configure_pdf_cover_process(std::env::current_exe().expect("could not locate desktop executable"));
    let backend_context = BackendLaunch::initialize(AppDataLocation::SharedDesktop).expect("failed to initialize backend app-data directory");
    app::init_logging();

    #[cfg(target_os = "windows")]
    if desktop_gpui::windows_updates::helper_entry(backend_context.app_data_dir()).expect("failed to run update helper") {
        return;
    }
    #[cfg(target_os = "windows")]
    let launch = match desktop_gpui::windows_updates::enter(backend_context.app_data_dir()).expect("failed to establish startup ownership") {
        Some(launch) => launch,
        None => return,
    };

    let instance = desktop_gpui::DesktopInstance::acquire(backend_context.app_data_dir()).expect("failed to establish desktop application ownership");
    let desktop_gpui::DesktopInstance::Primary(instance) = instance else {
        return;
    };
    #[cfg(target_os = "windows")]
    {
        let trial = launch.acquired().expect("failed to acknowledge startup ownership");
        if !desktop_gpui::windows_updates::initialize(backend_context.app_data_dir(), trial.as_deref()).expect("failed to recover application update") {
            return;
        }
    }
    #[cfg(target_os = "linux")]
    match desktop_gpui::linux_updates::initialize(backend_context.app_data_dir()).expect("failed to recover application update") {
        desktop_gpui::linux_updates::Startup::Run => {}
        desktop_gpui::linux_updates::Startup::Relaunch { executable, trial } => {
            drop(instance);
            if !desktop_gpui::linux_updates::launch(backend_context.app_data_dir(), &executable, trial.as_deref()).expect("failed to supervise application startup") {
                // No backend has started in this process. Re-enter under desktop
                // ownership and restore the durable journal before opening any data.
                if trial.is_some() {
                    main();
                }
            }
            return;
        }
    }
    let activations = instance.activations.clone();

    let _instance = instance;
    #[cfg(target_os = "linux")]
    let startup_permit = desktop_gpui::linux_updates::trial_pending().then(app::host::defer_background_work);
    #[cfg(target_os = "windows")]
    let startup_permit = desktop_gpui::windows_updates::trial_pending().then(app::host::defer_background_work);
    gpui_platform::application().with_assets(ui_components::Assets).run(move |cx| {
        let opened = desktop_gpui::open_ui(backend_context, desktop_gpui::UiLaunchOptions::DESKTOP, None, Some(activations), cx);
        #[cfg(target_os = "windows")]
        if opened.is_some() {
            desktop_gpui::windows_updates::healthy().expect("failed to commit healthy update startup");
            if let Some(permit) = startup_permit { permit.release(); }
        }
        #[cfg(target_os = "linux")]
        if opened.is_some() {
            desktop_gpui::linux_updates::healthy().expect("failed to commit healthy update startup");
            if let Some(permit) = startup_permit {
                permit.release();
            }
        }
    });
}
