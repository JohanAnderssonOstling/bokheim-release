//! Inspect durable results or retry due audiobook jobs through the metadata server.
//! cargo run --release -p library-backend --example audiobook_enrichment -- DB status
//! cargo run --release -p library-backend --example audiobook_enrichment -- DB retry [URL]
use metadata_contract::audible::{LookupResponse, LookupStatus};

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() < 3 || args.len() > 5 || !matches!(args[2].as_str(), "status" | "retry" | "refresh") {
        return Err("usage: audiobook_enrichment LIBRARY_DB status|retry|refresh [METADATA_URL [CONTENT_HASH]]".into());
    }
    if !std::path::Path::new(&args[1]).is_file() {
        return Err("library database does not exist".into());
    }
    let db = library_database::Database::open(&args[1])?;
    if args[2] != "status" {
        let client = metadata_client::MetadataClient::new(args.get(3).map(String::as_str).unwrap_or("https://meta.bokheim.se"))?;
        let snapshot = db.upload_preparation_snapshot()?;
        let batch = db.prepared_upload_batch(&snapshot.hashes())?;
        let mut hashes = batch.hashes().into_iter().collect::<Vec<_>>();
        if let Some(selected) = args.get(4) {
            hashes.retain(|hash| hash.as_str() == selected);
            if hashes.is_empty() {
                return Err("selected content hash is not in the visible library".into());
            }
        }
        let refresh = args[2] == "refresh";
        let candidates = if refresh { hashes } else { db.audible_batch_candidates(now(), &hashes)? };
        for hash in candidates {
            let job = if refresh { db.claim_audiobook_refresh(now(), hash)? } else { db.claim_audible_batch(now(), hash)? };
            let Some(job) = job else { continue };
            let response = client.audible_lookup(&job.request).await.unwrap_or_else(|error| LookupResponse {
                status: LookupStatus::Unavailable,
                candidates: vec![],
                selected_asin: None,
                selected_region: None,
                chapter_plan: None,
                used_website_fallback: false,
                detail: error.to_string(),
            });
            let committed = db.resolve_audible_batch(&job, &response, now() + 300, now())?;
            eprintln!("{}: {:?}; committed={committed}; {}", job.request.title, response.status, response.detail);
        }
    }
    println!("{}", serde_json::to_string_pretty(&db.audiobook_enrichment_status()?)?);
    Ok(())
}
