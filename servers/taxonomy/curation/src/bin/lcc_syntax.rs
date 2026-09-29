//! Distinguish malformed classification strings from taxonomy coverage gaps.
use subject_projection::canonical_lcc_notation;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("usage: lcc_syntax CODE_USAGE.sqlite3")?;
    let connection = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut query = connection.prepare("SELECT notation FROM code_usage WHERE scheme=2 ORDER BY notation")?;
    let mut output = csv::Writer::from_writer(std::io::stdout());
    output.write_record(["code", "valid_syntax"])?;
    for row in query.query_map([], |row| row.get::<_, String>(0))? {
        let code = row?;
        output.write_record([code.as_str(), if canonical_lcc_notation(&code).is_some() { "true" } else { "false" }])?;
    }
    output.flush()?;
    Ok(())
}
