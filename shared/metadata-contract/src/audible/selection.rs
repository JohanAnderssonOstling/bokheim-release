//! Recording selection uses titles and measured runtime before supporting credits.
use super::*;
use crate::matching::{author_match_tokens, bibliographic_match_key as key};

fn title_key(value: &str) -> String {
    let value = value.trim();
    let lower = value.to_ascii_lowercase();
    let value = [" (unabridged)", " (abridged)"].iter().find(|suffix| lower.ends_with(**suffix)).map_or(value, |suffix| &value[..value.len() - suffix.len()]);
    let mut value = crate::matching::NormalizedTitle::new(value).full;
    for suffix in [" revised and updated edition", " revised edition", " updated edition"] {
        if let Some(title) = value.strip_suffix(suffix) {
            value = title.to_owned();
        }
    }
    value
}

/// Extra discovery query only; the prefix is not assumed to be an author.
pub fn split_title_search_hint(value: &str) -> Option<(String, String)> {
    let value = value.replace('_', " ");
    [" - ", " – ", " — "].iter().find_map(|separator| {
        let (prefix, title) = value.split_once(separator)?;
        let (prefix, title) = (prefix.trim(), title.trim());
        (!prefix.is_empty() && key(title).chars().count() >= 8).then(|| (prefix.to_owned(), title.to_owned()))
    })
}

pub fn title_score(request: &LookupRequest, candidate: &Candidate) -> u16 {
    // A filename prefix is only discarded after the catalogue author verifies it.
    let hint = split_title_search_hint(&request.title);
    let verified_title = hint
        .as_ref()
        .filter(|(prefix, _)| {
            let tokens = author_match_tokens(prefix);
            !tokens.is_empty() && candidate.authors.iter().any(|author| author_match_tokens(author) == tokens)
        })
        .map(|(_, title)| title.as_str())
        .unwrap_or(&request.title);
    let local = title_key(verified_title);
    let remote = title_key(&candidate.title);
    if local.is_empty() || remote.is_empty() {
        return 0;
    }
    let main_local = title_key(verified_title.split(':').next().unwrap_or_default());
    let main_remote = title_key(candidate.title.split(':').next().unwrap_or_default());
    let local_full = title_key(&format!("{} {}", verified_title, request.recording.subtitle.as_deref().unwrap_or_default()));
    let remote_full = title_key(&format!("{} {}", candidate.title, candidate.subtitle.as_deref().unwrap_or_default()));
    let local_subtitle = request.recording.subtitle.as_deref().or_else(|| verified_title.split_once(':').map(|(_, s)| s));
    let remote_subtitle = candidate.subtitle.as_deref().or_else(|| candidate.title.split_once(':').map(|(_, s)| s));
    if local_subtitle.zip(remote_subtitle).is_some_and(|(l, r)| title_key(l) != title_key(r)) {
        return 850;
    }
    if local_full == remote_full || local == remote {
        return 1000;
    }
    // A subtitle can be absent locally, but do not identify works by a generic
    // substring such as "the" or "volume". Full main titles must agree.
    if main_local == main_remote && main_local.chars().count() >= 8 {
        let ls = request.recording.subtitle.as_deref().or_else(|| verified_title.split_once(':').map(|(_, s)| s));
        let rs = candidate.subtitle.as_deref().or_else(|| candidate.title.split_once(':').map(|(_, s)| s));
        return if ls.zip(rs).is_some_and(|(l, r)| key(l) != title_key(r)) { 850 } else { 980 };
    }
    0
}

fn credits_match(local: &[String], remote: &[String]) -> bool {
    local.iter().any(|l| {
        let a = author_match_tokens(l);
        remote.iter().any(|r| {
            let b = author_match_tokens(r);
            // Surname overlap alone is not affirmative author evidence.
            !a.is_empty() && (a == b || (a.len() >= 2 && a.is_subset(&b)) || (b.len() >= 2 && b.is_subset(&a)))
        })
    })
}

fn recording_conflict(request: &LookupRequest, candidate: &Candidate) -> bool {
    let local = key(&format!("{} {}", request.title, request.recording.subtitle.as_deref().unwrap_or_default()));
    let remote = key(&format!("{} {}", candidate.title, candidate.subtitle.as_deref().unwrap_or_default()));
    // Explicit production types matter. Never mine descriptions for publishers.
    ["dramatization", "dramatisation", "full cast", "radio drama"].iter().any(|clue| local.contains(clue) != remote.contains(clue))
        || (local.contains("unabridged") && candidate.abridged == Some(true))
        || (local.split_whitespace().any(|s| s == "abridged") && candidate.abridged == Some(false))
}

pub fn runtime_close(local: u64, remote: u64) -> bool {
    local > 0 && remote > 0 && local.abs_diff(remote) <= 30_000.max(local / 100)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selection {
    Supported(usize),
    Ambiguous,
    Unavailable,
    NoMatch,
}

/// Call only after fetching precise chapter trees for plausible candidates.
pub fn select(request: &LookupRequest, candidates: &[Candidate]) -> Selection {
    let mut ranked = Vec::new();
    let mut unresolved = false;
    for (index, candidate) in candidates.iter().enumerate() {
        let title = title_score(request, candidate);
        if title < 950 || recording_conflict(request, candidate) {
            continue;
        }
        let Some(duration) = candidate.duration_ms.filter(|d| *d > 0) else {
            unresolved = true;
            continue;
        };
        let author = credits_match(&request.authors, &candidate.authors);
        let narrator = credits_match(&request.recording.narrators, &candidate.narrators);
        let embedded = request.recording.asins.iter().any(|a| a.eq_ignore_ascii_case(&candidate.asin));
        let close = runtime_close(request.duration_ms, duration);
        let equal = request.chapters.len() > 1 && request.chapters.len() == candidate.chapters.len();
        // Names-only enrichment can identify a known edition without penalizing
        // runtime. Importing boundaries will still require recording alignment.
        if !close && !(equal && (author || narrator || embedded)) && !super::chapters::recording_alignment(request, candidate) {
            continue;
        }
        let date = request.recording.recording_release_date.as_ref().zip(candidate.release_date.as_ref()).is_some_and(|(l, r)| l == r)
            || request.recording.recording_release_year.zip(candidate.release_date.as_deref().and_then(|date| date.get(..4).and_then(|year| year.parse::<i32>().ok()))).is_some_and(|(local, remote)| local == remote);
        let distance = request.duration_ms.abs_diff(duration);
        ranked.push((index, embedded, close, distance, title, author, narrator, date));
    }
    // A plausible candidate with a failed chapter lookup is not negative
    // evidence. Resolve it before declaring another recording uniquely supported.
    if unresolved {
        return Selection::Unavailable;
    }
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.cmp(&a.2)).then(a.3.cmp(&b.3)).then(b.4.cmp(&a.4)));
    let Some(best) = ranked.first() else {
        return Selection::NoMatch;
    };
    let mut tied = vec![best.0];
    for other in ranked.iter().skip(1) {
        if other.1 != best.1 || other.2 != best.2 {
            continue;
        }
        // Runtime differences inside a small window do not prove an edition.
        // Where timings will not be applied, omit duration from the tie.
        if best.2 && other.3.abs_diff(best.3) > 10_000.max(request.duration_ms / 1000) {
            continue;
        }
        let a = &candidates[best.0];
        let b = &candidates[other.0];
        let regional = key(&a.title) == key(&b.title)
            && credits_match(&a.authors, &b.authors)
            && credits_match(&a.narrators, &b.narrators)
            && a.duration_ms.zip(b.duration_ms).is_some_and(|(x,y)| x.abs_diff(y) <= 1000)
            && (a.region != b.region || a.asin == b.asin)
            && a.chapters.len() == b.chapters.len()
            // Regional encodings can differ by a small fraction of a second.
            // Require every label and boundary to agree, not merely total runtime.
            && a.chapters.iter().zip(&b.chapters).all(|(x,y)| x.title == y.title
                && x.start_ms.abs_diff(y.start_ms) <= 250 && x.end_ms.abs_diff(y.end_ms) <= 250);
        if !regional {
            tied.push(other.0);
        }
    }
    if tied.len() == 1 {
        return Selection::Supported(best.0);
    }
    // Supporting credits resolve ties, and only an actual recording release
    // date is the final tie-breaker. A raw local book year is excluded.
    for field in [5, 6, 7] {
        let supported = tied
            .iter()
            .copied()
            .filter(|index| {
                let r = ranked.iter().find(|r| r.0 == *index).unwrap();
                match field {
                    5 => r.5,
                    6 => r.6,
                    _ => r.7,
                }
            })
            .collect::<Vec<_>>();
        if supported.len() == 1 {
            return Selection::Supported(supported[0]);
        }
        if !supported.is_empty() {
            tied = supported;
        }
    }
    Selection::Ambiguous
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filename_prefix_requires_matching_catalogue_author() {
        let mut r = request();
        r.title = "John_Smith_-_A_Specific_Book".into();
        r.authors.clear();
        let c = candidate("B000000001", 600_000);
        assert_eq!(title_score(&r, &c), 1000);
        assert_eq!(select(&r, &[c.clone()]), Selection::Supported(0));
        assert!(r.authors.is_empty(), "discovery must not manufacture local author evidence");
        r.title = "Another Author - A Specific Book".into();
        assert_eq!(title_score(&r, &c), 0);
        assert_eq!(select(&r, &[c.clone()]), Selection::NoMatch);
        r.title = "John Smith - A Different Book".into();
        assert_eq!(title_score(&r, &c), 0);
        assert!(split_title_search_hint("A Well-Known Book").is_none());
    }
    fn request() -> LookupRequest {
        LookupRequest {
            title: "A Specific Book".into(),
            authors: vec!["John Q. Smith".into()],
            duration_ms: 600_000,
            recording: RecordingEvidence::default(),
            chapters: vec![Chapter { title: "Full audiobook".into(), start_ms: 0, end_ms: 600_000 }],
        }
    }
    fn candidate(asin: &str, duration: u64) -> Candidate {
        Candidate {
            asin: asin.into(),
            region: "us".into(),
            title: "A Specific Book".into(),
            subtitle: None,
            authors: vec!["John Smith".into()],
            narrators: vec!["Narrator One".into()],
            publisher: None,
            description: None,
            release_date: None,
            isbns: vec![],
            abridged: None,
            provider_duration_ms: Some(9_999_000),
            duration_ms: Some(duration),
            chapters: vec![Chapter { title: "A descriptive chapter".into(), start_ms: 0, end_ms: duration }],
            metadata_provider: "audible".into(),
            chapter_provider: Some("audible".into()),
            source: DiscoverySource::Api,
        }
    }
    #[test]
    fn precise_duration_outweighs_mangled_local_credits() {
        let mut correct = candidate("B000000001", 600_000);
        correct.authors = vec!["Different local credit spelling".into()];
        assert_eq!(select(&request(), &[candidate("B000000002", 900_000), correct]), Selection::Supported(1));
    }
    #[test]
    fn ambiguous_recordings_need_evidence_and_only_recording_date_breaks_tie() {
        let mut request = request();
        request.recording.date = Some("2020-01-01".into());
        let a = candidate("B000000001", 600_000);
        let mut b = candidate("B000000002", 605_000);
        b.release_date = Some("2020-01-01".into());
        assert_eq!(select(&request, &[a.clone(), b.clone()]), Selection::Ambiguous);
        request.recording.recording_release_date = request.recording.date.clone();
        assert_eq!(select(&request, &[a, b]), Selection::Supported(1));
    }
    #[test]
    fn equal_count_names_do_not_require_runtime_agreement() {
        let mut request = request();
        request.chapters.push(request.chapters[0].clone());
        let mut candidate = candidate("B000000001", 900_000);
        candidate.chapters.push(candidate.chapters[0].clone());
        assert_eq!(select(&request, &[candidate]), Selection::Supported(0));
    }
    #[test]
    fn regional_encodings_with_matching_labels_and_subsecond_boundaries_are_one_recording() {
        let r = request();
        let a = candidate("B000000001", 600_000);
        let mut b = candidate("B000000002", 600_232);
        b.region = "uk".into();
        assert_eq!(select(&r, &[a.clone(), b.clone()]), Selection::Supported(0));
        b.chapters[0].title = "A different chapter".into();
        assert_eq!(select(&r, &[a.clone(), b.clone()]), Selection::Ambiguous);
        b.chapters[0].title = a.chapters[0].title.clone();
        b.chapters[0].end_ms = 601_001;
        b.duration_ms = Some(601_001);
        assert_eq!(select(&r, &[a, b]), Selection::Ambiguous);
    }

    #[test]
    fn explicit_format_annotation_is_not_part_of_the_title_but_still_constrains_the_recording() {
        let mut r = request();
        r.title.push_str(" (Unabridged)");
        let mut c = candidate("B000000001", 600_000);
        c.abridged = Some(false);
        assert_eq!(select(&r, &[c.clone()]), Selection::Supported(0));
        c.abridged = Some(true);
        assert_eq!(select(&r, &[c]), Selection::NoMatch);
    }

    #[test]
    fn production_conflict_and_generic_title_substrings_are_rejected() {
        let mut c = candidate("B000000001", 600_000);
        c.subtitle = Some("A full cast dramatisation".into());
        assert_eq!(select(&request(), &[c.clone()]), Selection::NoMatch);
        c.title = "Book".into();
        c.subtitle = None;
        assert_eq!(select(&request(), &[c]), Selection::NoMatch);
    }
    #[test]
    fn revised_edition_and_author_initials_are_accepted() {
        let mut c = candidate("B000000001", 600_000);
        c.title.push_str(" (Revised Edition)");
        assert_eq!(select(&request(), &[c]), Selection::Supported(0));
    }
    #[test]
    fn unresolved_plausible_recording_blocks_selection_until_lookup_recovers() {
        let r = request();
        let mut missing = candidate("B000000001", 600_000);
        missing.duration_ms = None;
        missing.chapters.clear();
        let supported = candidate("B000000002", 600_000);
        assert_eq!(select(&r, &[missing.clone(), supported.clone()]), Selection::Unavailable);
        missing.duration_ms = Some(900_000);
        assert_eq!(select(&r, &[missing, supported]), Selection::Supported(1));
    }

    #[test]
    fn missing_duration_for_an_unrelated_title_does_not_block_selection() {
        let r = request();
        let mut missing = candidate("B000000001", 600_000);
        missing.title = "An unrelated book".into();
        missing.duration_ms = None;
        assert_eq!(select(&r, &[missing, candidate("B000000002", 600_000)]), Selection::Supported(1));
    }
}
