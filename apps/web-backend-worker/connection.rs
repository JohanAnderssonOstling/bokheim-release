//! Shared application deployment configuration used by the web UI and worker probes.
pub fn configuration(broker_url: &str) -> app::host::web::WebConfiguration {
    // Every coordinator and dedicated worker belongs to this tab's immutable bundle.
    let base = broker_url.rsplit_once('/').map(|(base, _)| format!("{base}/")).unwrap_or_default();
    app::host::web::WebConfiguration {
        broker_url: broker_url.to_owned(),
        broker_name: "bokheim-backend-hosted-v3".into(),
        lifetime_lock: format!("bokheim-tab-{}", uuid::Uuid::new_v4()),
        worker_name_prefix: "bokheim-tab-worker".into(),
        workers: [("database", "web_backend_database_worker_loader.js?endpoint"), ("cpu", "cpu_worker_loader.js?endpoint"), ("import", "import_worker_loader.js?endpoint")]
            .into_iter()
            .map(|(role, script)| (role.into(), format!("{base}{script}")))
            .collect(),
    }
}
