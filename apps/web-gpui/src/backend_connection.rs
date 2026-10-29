//! Application deployment identity and worker script URLs.
pub fn configuration(broker_url: &str) -> app::host::web::WebConfiguration {
    app::host::web::backend_configuration(broker_url)
}
