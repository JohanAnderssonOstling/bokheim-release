//! Offline publisher: cargo run --release -p update-client --example sign_manifest --
//! manifest.json private-key.pk8 key-id signed.json
//! Keep the signing key outside the repository and the hosting environment.
use ring::signature::{Ed25519KeyPair, KeyPair};
use std::{collections::BTreeMap, env, fs};
use update_client::{DiscoveryConfig, Manifest, SignedManifest, SIGNATURE_CONTEXT};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 4 {
        return Err("usage: sign_manifest manifest.json private-key.pk8 key-id signed.json".into());
    }
    let payload = fs::read_to_string(&args[0])?;
    let manifest: Manifest = serde_json::from_str(&payload)?;
    let key_bytes = fs::read(&args[1])?;
    // OpenSSL writes PKCS#8 v1 without the optional public key. Ring derives it
    // from the private seed; v2 keys still have their included public key checked.
    let key = Ed25519KeyPair::from_pkcs8_maybe_unchecked(&key_bytes).map_err(|_| "invalid Ed25519 PKCS#8 signing key")?;
    let mut message = SIGNATURE_CONTEXT.to_vec();
    message.extend_from_slice(payload.as_bytes());
    let signature = key.sign(&message).as_ref().iter().map(|b| format!("{b:02x}")).collect();
    let envelope = SignedManifest { key_id: args[2].clone(), payload, signature };
    let bytes = serde_json::to_vec(&envelope)?;
    let config = DiscoveryConfig { endpoint: update_client::DEFAULT_ENDPOINT.into(), channel: manifest.channel.clone(), trusted_keys: BTreeMap::from([(args[2].clone(), key.public_key().as_ref().try_into()?)]) };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
    update_client::verify(&bytes, &config, now, 0)?;
    fs::write(&args[3], bytes)?;
    Ok(())
}
