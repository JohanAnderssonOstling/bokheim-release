//! Offline re-encoding of existing relay payloads using the shared client model.
//! See compact-sync-payloads.md for the export and conditional database update.
use library_replica::LibraryStateEntry;
use std::error::Error;
use std::io::{self, BufRead, Write};
use sync_common::{transport, LibraryId};

fn unhex(text: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    if text.len() % 2 != 0 || text.len() > transport::MAX_DECODED_REQUEST_BYTES * 2 {
        return Err("invalid payload hex length".into());
    }
    text.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let digit = |byte: u8| (byte as char).to_digit(16).ok_or("invalid hex digit");
            Ok((digit(pair[0])? * 16 + digit(pair[1])?) as u8)
        })
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(DIGITS[(byte >> 4) as usize] as char);
        text.push(DIGITS[(byte & 15) as usize] as char);
    }
    text
}

fn compact(line: &str) -> Result<Option<String>, Box<dyn Error>> {
    let parts = line.split('\t').collect::<Vec<_>>();
    if parts.len() != 4 {
        return Err("expected library_id, kind, entity_key hex, payload hex".into());
    }
    LibraryId::parse_str(parts[0])?;
    let key = unhex(parts[2])?;
    let previous = unhex(parts[3])?;
    let state: LibraryStateEntry = transport::decode_state(&previous, transport::MAX_DECODED_REQUEST_BYTES)?;
    match &state {
        LibraryStateEntry::BookAdd { .. } | LibraryStateEntry::ExternalBook { .. } => {}
        _ => return Err("expected an immutable or provider book payload".into()),
    }
    let address = state.address();
    if address.kind != parts[1] || address.entity_key.as_bytes() != key || !address.entity_subkey.is_empty() {
        return Err("payload identity differs from its stored address".into());
    }
    state.validate()?;
    let encoded = transport::encode(&state)?;
    if encoded == previous {
        return Ok(None);
    }
    let decoded: LibraryStateEntry = transport::decode_state(&encoded, transport::MAX_DECODED_REQUEST_BYTES)?;
    if decoded != state {
        return Err("re-encoding changed the book state".into());
    }
    Ok(Some(format!("{}\t{}\t{}\t{}\t{}", parts[0], parts[1], parts[2], parts[3], hex(&encoded))))
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut output = io::BufWriter::new(io::stdout().lock());
    let mut changed = 0;
    for line in io::stdin().lock().lines() {
        if let Some(row) = compact(&line?)? {
            writeln!(output, "{row}")?;
            changed += 1;
        }
    }
    output.flush()?;
    eprintln!("Re-encoded {changed} book payloads; no database was modified.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use library_replica::{BookAddition, BookFacts, EnrichmentResult};
    use sync_common::BookId;

    #[test]
    fn already_compact_pdf_is_idempotent_and_mismatched_addresses_are_rejected() {
        let hash = BookId::from_bytes([1; 32]);
        let value = BookAddition {
            facts: BookFacts { added_at: 1, format: book_model::BookFormat::Pdf },
            record: book_model::BookRecord { metadata: Default::default(), description: String::new() },
            navigation: None,
            pdf: Some(pdf_view_common::PdfReaderMetadata {
                version: 1,
                content_hash: hash.to_string(),
                pages: vec![pdf_view_common::PdfStoredPage { width: 600.0, height: 800.0, rotation: 0 }],
                skippable_pages: None,
                annotations: vec![],
                dependencies: None,
            }),
            recording: None,
            enrichment: Some(EnrichmentResult::NoMatch),
        };
        let state = LibraryStateEntry::BookAdd { content_hash: hash, value };
        let bytes = transport::encode(&state).unwrap();
        let prefix = format!("00000000-0000-0000-0000-000000000001\tbook_add\t{}", hex(hash.as_bytes()));
        assert!(compact(&format!("{prefix}\t{}", hex(&bytes))).unwrap().is_none());
        let mut legacy_version = bytes.clone();
        legacy_version[1] = 30;
        let changed = compact(&format!("{prefix}\t{}", hex(&legacy_version))).unwrap().unwrap();
        assert_eq!(changed.split('\t').last().unwrap(), hex(&bytes));
        assert!(compact(&format!("00000000-0000-0000-0000-000000000001\tbook_add\t00\t{}", hex(&bytes))).is_err());
    }
}
