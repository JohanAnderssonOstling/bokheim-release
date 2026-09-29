//! Build-source compatibility metadata for APK verification without a device.
fn main() -> Result<(), String> {
    let version = std::env::args().nth(1).ok_or("expected application version")?;
    let device = linux_update_host::device(&version, None)?;
    let config = update_client::DiscoveryConfig::bundled()?;
    let mut info = serde_json::to_value(device).map_err(|error| error.to_string())?;
    info["update_trust"] = serde_json::json!({
        "endpoint": config.endpoint,
        "channel": config.channel,
        "keys": config.trusted_keys.iter().map(|(id, key)|
            (id.clone(), key.iter().map(|b| format!("{b:02x}")).collect::<String>())
        ).collect::<std::collections::BTreeMap<_, _>>()
    });
    println!("{}", serde_json::to_string(&info).map_err(|error| error.to_string())?);
    Ok(())
}
