//! Backfill only missing artwork from previously selected audiobook recordings.
//! cargo run --release -p library-backend --example audiobook_covers -- DB LIBRARY_ROOT [METADATA_URL]
use library_backend::{asset_store::AssetStore, BlobKind};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args: Vec<_> = std::env::args().collect();
    if !(3..=4).contains(&args.len()) || !std::path::Path::new(&args[1]).is_file() || !std::path::Path::new(&args[2]).is_dir() {
        return Err("usage: audiobook_covers EXISTING_LIBRARY_DB EXISTING_LIBRARY_ROOT [METADATA_URL]".into());
    }
    let db = library_database::Database::open(&args[1])?;
    let assets = AssetStore::open(&args[2])?;
    let client = metadata_client::MetadataClient::new(args.get(3).map(String::as_str).unwrap_or("https://meta.bokheim.se"))?;
    let scope = serde_json::to_string(&db.upload_preparation_snapshot()?.hashes())?;
    let mut cursor = String::new();
    let mut updated = 0;
    loop {
        let candidates = db.audible_cover_candidates("audible_covers_v1", &cursor, &scope)?;
        if candidates.is_empty() {
            break;
        }
        for (hash, asin, region) in candidates {
            cursor = hash.to_string();
            let identity = (asin.clone(), region.clone());
            let expected = db.cover_enrichment_state(&hash)?;
            if db.audible_cover_identity(&hash)? != Some(identity.clone()) || assets.read_bytes(BlobKind::Thumbnail, &hash).await?.is_some() {
                continue;
            }
            let Some(bytes) = client.audible_cover(&asin, &region).await? else {
                eprintln!("{hash}: provider has no artwork");
                continue;
            };
            let versions = thumbnail::generate_thumbnail_versions_from_image_bytes(&bytes)?;
            let _lease = assets.lease_book(&hash).await?;
            if db.cover_enrichment_state(&hash)? != expected || db.audible_cover_identity(&hash)? != Some(identity) || assets.read_bytes(BlobKind::Thumbnail, &hash).await?.is_some() {
                continue;
            }
            assets.write(BlobKind::Thumbnail, &hash, &versions.high_density).await?;
            assets.write_thumbnail_variant(&hash, thumbnail::BROWSE_THUMBNAIL_WIDTH, &versions.browse).await?;
            db.complete_thumbnail_work(&hash, true)?;
            db.record_enrichment_attempts(vec![library_database::EnrichmentAttempt { content_hash: hash, provider: "audible_covers_v1".into(), identifier: format!("{region}:{asin}"), status: "updated".into(), detail: None }])?;
            updated += 1;
            println!("{hash}: added artwork from {region}/{asin}");
        }
    }
    println!("Added {updated} missing audiobook covers");
    Ok(())
}
