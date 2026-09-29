//! Metadata from the compiled runtime, shared by desktop and Android probes.
pub fn json() -> Result<String, String> {
    let device = linux_update_host::device(env!("CARGO_PKG_VERSION"), None)?;
    let mut info = serde_json::to_value(device).map_err(|error| error.to_string())?;
    info["update_trust"] = match update_client::DiscoveryConfig::bundled() {
        Ok(config) => serde_json::json!({
            "endpoint": config.endpoint,
            "channel": config.channel,
            "keys": config.trusted_keys.iter().map(|(id, key)|
                (id.clone(), key.iter().map(|b| format!("{b:02x}")).collect::<String>())
            ).collect::<std::collections::BTreeMap<_, _>>()
        }),
        Err(_) => serde_json::Value::Null,
    };
    serde_json::to_string(&info).map_err(|error| error.to_string())
}
