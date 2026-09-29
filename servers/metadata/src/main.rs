use metadata_server::{app, build_subject_index_limited, import_description_snapshot, import_snapshot, split_snapshot, MetadataService};
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tracing_subscriber::{fmt, EnvFilter};

const DEFAULT_BIND: &str = "127.0.0.1:8091";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    fmt().with_env_filter(EnvFilter::from_default_env()).init();
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    if arguments.first().is_some_and(|argument| argument == "rebuild-titles") {
        if arguments.len() != 5 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "rebuild-titles SOURCE EDITIONS.gz WORKS.gz OUTPUT").into());
        }
        metadata_server::rebuild_titles(Path::new(&arguments[1]), Path::new(&arguments[2]), Path::new(&arguments[3]), Path::new(&arguments[4]))?;
        return Ok(());
    }
    if arguments.first().is_some_and(|argument| argument == "lookup-lcc") {
        if arguments.len() < 3 {
            return Err(invalid_usage().into());
        }
        let service = MetadataService::open(Path::new(&arguments[1]))?;
        let isbns = arguments[2..].iter().map(|s| s.to_string_lossy().into_owned()).collect::<Vec<_>>();
        let mut results = Vec::new();
        for chunk in isbns.chunks(64) {
            results.extend(service.lookup(metadata_contract::ClassificationRequest { isbns: chunk.to_vec() })?.results);
        }
        println!("{}", serde_json::to_string(&results)?);
        return Ok(());
    }
    if arguments.first().is_some_and(|argument| argument == "librarything-lookup" || argument == "librarything-refresh-schemes") {
        if arguments.len() != 2 {
            return Err(invalid_usage().into());
        }
        let key = std::env::var_os("BOKHEIM_LIBRARYTHING_KEY_FILE").ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "BOKHEIM_LIBRARYTHING_KEY_FILE is required"))?;
        let cache = std::env::var_os("BOKHEIM_LIBRARYTHING_CACHE").ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "BOKHEIM_LIBRARYTHING_CACHE is required"))?;
        let result = if arguments[0] == "librarything-refresh-schemes" {
            metadata_server::refresh_librarything_schemes(Path::new(&key), Path::new(&cache), &arguments[1].to_string_lossy()).await?
        } else {
            metadata_server::lookup_librarything_isbn(Path::new(&key), Path::new(&cache), &arguments[1].to_string_lossy()).await?
        };
        println!("{}", serde_json::to_string(&result)?);
        return Ok(());
    }
    if arguments.first().is_some_and(|argument| argument == "benchmark-subjects") {
        if arguments.len() != 6 {
            return Err(invalid_usage().into());
        }
        let database = Path::new(&arguments[1]);
        let iterations = arguments[3].to_string_lossy().parse::<usize>().map_err(|_| invalid_usage())?;
        let offset = arguments[4].to_string_lossy().parse::<usize>().map_err(|_| invalid_usage())?;
        let count = arguments[5].to_string_lossy().parse::<usize>().map_err(|_| invalid_usage())?;
        let connection = rusqlite::Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut statement = connection.prepare("SELECT DISTINCT isbn13 FROM edition_isbn ORDER BY isbn13 LIMIT ?1 OFFSET ?2")?;
        let isbns = statement.query_map((i64::try_from(count)?, i64::try_from(offset)?), |row| row.get::<_, i64>(0))?.map(|isbn| isbn.map(|isbn| format!("{isbn:013}"))).collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        drop(connection);
        let mut service = MetadataService::open(database)?;
        if arguments[2] != "-" {
            service = service.with_subject_index(PathBuf::from(&arguments[2]))?;
        }
        let started = Instant::now();
        let mut subjects = 0usize;
        for _ in 0..iterations {
            let response = service.lookup(metadata_contract::ClassificationRequest { isbns: isbns.clone() })?;
            subjects += response.results.iter().flat_map(|result| &result.matches).map(|matched| matched.classifications.len()).sum::<usize>();
        }
        let seconds = started.elapsed().as_secs_f64();
        let lookups = iterations * isbns.len();
        println!("{{\"lookups\":{lookups},\"subjects\":{subjects},\"seconds\":{seconds:.6},\"lookups_per_second\":{:.2},\"subjects_per_second\":{:.2}}}", lookups as f64 / seconds, subjects as f64 / seconds);
        return Ok(());
    }
    if arguments.first().is_some_and(|argument| argument == "build-subject-index") {
        if arguments.len() < 3 || arguments.len() > 4 {
            return Err(invalid_usage().into());
        }
        let limit = arguments.get(3).map(|value| value.to_string_lossy().parse::<usize>()).transpose().map_err(|_| invalid_usage())?;
        build_subject_index_limited(Path::new(&arguments[1]), Path::new(&arguments[2]), limit)?;
        return Ok(());
    }
    if arguments.first().is_some_and(|argument| argument == "split-snapshot") {
        if arguments.len() != 5 {
            return Err(invalid_usage().into());
        }
        split_snapshot(Path::new(&arguments[1]), Path::new(&arguments[2]), Path::new(&arguments[3]), Path::new(&arguments[4]))?;
        return Ok(());
    }
    if arguments.first().is_some_and(|argument| argument == "import") {
        if arguments.len() < 5 || arguments.len() > 6 {
            return Err(invalid_usage().into());
        }
        let limit = arguments.get(5).map(|value| value.to_string_lossy().parse::<usize>()).transpose().map_err(|_| invalid_usage())?;
        import_snapshot(Path::new(&arguments[1]), Path::new(&arguments[2]), Path::new(&arguments[3]), Path::new(&arguments[4]), limit)?;
        return Ok(());
    }
    if arguments.first().is_some_and(|argument| argument == "import-descriptions") {
        if arguments.len() < 4 || arguments.len() > 5 {
            return Err(invalid_usage().into());
        }
        let limit = arguments.get(4).map(|value| value.to_string_lossy().parse::<usize>()).transpose().map_err(|_| invalid_usage())?;
        import_description_snapshot(Path::new(&arguments[1]), Path::new(&arguments[2]), &arguments[3].to_string_lossy(), limit)?;
        return Ok(());
    }
    if arguments.first().is_some_and(|a| a == "ingest-wikidata") {
        if arguments.len() != 4 {
            return Err(invalid_usage().into());
        }
        let base = PathBuf::from(&arguments[1]);
        let descriptions = base.parent().map(|p| p.join("descriptions-current.sqlite")).filter(|p| p.is_file());
        let service = MetadataService::open_with_supplements(&base, descriptions)?;
        metadata_server::ingest_wikidata_authorities(&service, Path::new(&arguments[2]), Path::new(&arguments[3]))?;
        return Ok(());
    }
    if !arguments.is_empty() {
        return Err(invalid_usage().into());
    }

    let legacy_database = std::env::var_os("BOKHEIM_METADATA_DATABASE").map(PathBuf::from);
    let database = std::env::var_os("BOKHEIM_METADATA_SUBJECT_DATABASE")
        .map(PathBuf::from)
        .or_else(|| legacy_database.clone())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "BOKHEIM_METADATA_SUBJECT_DATABASE or BOKHEIM_METADATA_DATABASE is required"))?;
    let rich_database = std::env::var_os("BOKHEIM_METADATA_RICH_DATABASE").map(PathBuf::from).or_else(|| legacy_database.clone()).unwrap_or_else(|| database.clone());
    let identity_database = std::env::var_os("BOKHEIM_METADATA_IDENTITY_DATABASE").map(PathBuf::from).or(legacy_database).unwrap_or_else(|| database.clone());
    let description_database = std::env::var_os("BOKHEIM_METADATA_DESCRIPTION_DATABASE").map(PathBuf::from).or_else(|| database.parent().map(|parent| parent.join("descriptions-current.sqlite")).filter(|path| path.is_file()));
    let bind: SocketAddr = std::env::var("BOKHEIM_METADATA_BIND").unwrap_or_else(|_| DEFAULT_BIND.to_owned()).parse().map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, format!("invalid BOKHEIM_METADATA_BIND: {error}")))?;
    if !bind.ip().is_loopback() && std::env::var("BOKHEIM_METADATA_ALLOW_PUBLIC_HTTP").as_deref() != Ok("true") {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "non-loopback plain HTTP requires BOKHEIM_METADATA_ALLOW_PUBLIC_HTTP=true; use a TLS reverse proxy in production").into());
    }
    let mut service = MetadataService::open_with_stores(database, rich_database, identity_database, description_database)?;
    if let Some(index) = std::env::var_os("BOKHEIM_METADATA_SUBJECT_INDEX") {
        service = service.with_subject_index(PathBuf::from(index))?;
    }
    match (std::env::var_os("BOKHEIM_LIBRARYTHING_KEY_FILE"), std::env::var_os("BOKHEIM_LIBRARYTHING_CACHE")) {
        (Some(key), Some(cache)) => service = service.with_librarything(Path::new(&key), Path::new(&cache))?,
        (None, None) => {}
        _ => return Err(io::Error::new(io::ErrorKind::InvalidInput, "both BOKHEIM_LIBRARYTHING_KEY_FILE and BOKHEIM_LIBRARYTHING_CACHE are required").into()),
    }
    let loc_cache = std::env::var_os("BOKHEIM_LOC_CACHE").or_else(|| std::env::var_os("BOKHEIM_LIBRARYTHING_CACHE")).map(PathBuf::from);
    if let Some(path) = &loc_cache {
        service = service.with_cached_library_of_congress(path)?;
    }
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(address = %listener.local_addr()?, "metadata server listening");
    let background = if std::env::var("BOKHEIM_LIBRARYTHING_BACKGROUND").as_deref() == Ok("false") { None } else { service.start_librarything_background(std::env::var_os("BOKHEIM_LIBRARYTHING_RATINGS_DUMP").map(PathBuf::from)) };
    let loc_background = loc_cache.and_then(|p| service.start_library_of_congress_background(p));
    let result = axum::serve(listener, app(service)).with_graceful_shutdown(shutdown_signal()).await;
    if let Some(task) = background {
        task.abort();
    }
    if let Some(task) = loc_background {
        task.abort();
    }
    result?;
    Ok(())
}

fn invalid_usage() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "usage: metadata-server librarything-lookup ISBN\n       metadata-server build-subject-index SUBJECTS.sqlite SUBJECTS.idx [MAX_RECORDS]\n       metadata-server split-snapshot SOURCE.sqlite SUBJECTS.sqlite RICH.sqlite IDENTITIES.sqlite\n       metadata-server import EDITIONS.txt.gz WORKS.txt.gz AUTHORS.txt.gz OUTPUT.sqlite [MAX_RECORDS]\n       metadata-server import-descriptions WORKS.txt.gz OUTPUT.sqlite DUMP_DATE [MAX_RECORDS]\n       metadata-server  # configured with BOKHEIM_METADATA_SUBJECT_DATABASE and optional BOKHEIM_METADATA_SUBJECT_INDEX",
    )
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            signal.recv().await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = terminate => {} }
}
