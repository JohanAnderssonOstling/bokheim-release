//! Subject-enrichment phase order. Hosts execute steps and merge returned
//! evidence before requesting the next step. A failed request does not advance;
//! durable checkpoints resume at phase boundaries and may repeat work in a phase.
use crate::related_isbn;
use book_model::from_metadata_value;
use book_model::{BookMetadata, Scheme};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum SubjectPhase {
    #[default]
    MetadataCodes,
    MetadataIsbns,
    Pages,
    PageCodes,
    PageIsbns,
    TitleAuthor,
    FinalIsbns,
    Exhausted,
}

impl SubjectPhase {
    pub const ALL: [Self; 8] = [Self::MetadataCodes, Self::MetadataIsbns, Self::Pages, Self::PageCodes, Self::PageIsbns, Self::TitleAuthor, Self::FinalIsbns, Self::Exhausted];

    pub const fn checkpoint_id(self) -> u8 {
        self as u8
    }

    pub fn from_checkpoint_id(id: u8) -> Option<Self> {
        Self::ALL.get(usize::from(id)).copied()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubjectCode {
    pub authority: String,
    pub code: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubjectStep {
    ResolveCodes(Vec<SubjectCode>),
    LookupIsbns(Vec<String>),
    InspectPages,
    SearchTitleAuthor,
    Complete,
    /// All stages finished without resolved LCC. Keep any BISAC evidence.
    Exhausted,
}

#[derive(Clone, Debug, Default)]
pub struct SubjectPipeline {
    stage: SubjectPhase,
    checked_codes: BTreeSet<(String, String)>,
    checked_isbns: BTreeSet<String>,
    pending_codes: BTreeSet<(String, String)>,
    pending_isbns: BTreeSet<String>,
    pending: bool,
    pages_inspected: bool,
    pending_pages: bool,
}

impl SubjectPipeline {
    pub fn phase(&self) -> SubjectPhase {
        self.stage
    }

    /// Stores only the current phase. Work inside that phase may be repeated
    /// after a crash; checked-code and checked-ISBN sets remain in memory only.
    pub fn checkpoint_id(&self) -> u8 {
        self.stage.checkpoint_id()
    }

    /// Resume at the start of a phase, with no per-item progress restored.
    pub fn resume_at_phase(phase: SubjectPhase) -> Self {
        Self { stage: phase, ..Self::default() }
    }

    /// Probe without advancing a book into a later library-wide phase.
    pub fn next_step_in_phase(&mut self, metadata: &BookMetadata, phase: SubjectPhase) -> Option<SubjectStep> {
        let mut next = self.clone();
        let step = next.next_step(metadata);
        let actual = if matches!(step, SubjectStep::InspectPages) { SubjectPhase::Pages } else { next.stage };
        if actual != phase && !matches!(step, SubjectStep::Complete | SubjectStep::Exhausted) {
            // A phase probe must be side-effect free when the next available
            // step belongs to another phase, or a global wave scheduler could
            // silently skip the intervening phase.
            return None;
        }
        *self = next;
        Some(step)
    }

    pub fn next_step(&mut self, metadata: &BookMetadata) -> SubjectStep {
        if crate::has_resolved_lcc(metadata) {
            // Subject completion does not remove the independent need to find
            // an ISBN for authoritative author lookup.
            if !self.pages_inspected && crate::LookupPlan::from_metadata(metadata).author_isbns.is_empty() {
                self.pending = true;
                self.pending_pages = true;
                return SubjectStep::InspectPages;
            }
            self.pending = false;
            return SubjectStep::Complete;
        }
        loop {
            let step = match self.stage {
                SubjectPhase::MetadataCodes | SubjectPhase::PageCodes => {
                    let codes = metadata
                        .subjects
                        .iter()
                        .filter_map(|s| {
                            let authority = s.authority()?.to_ascii_lowercase();
                            let code = s.code()?.trim().to_owned();
                            (matches!(authority.as_str(), "lcc" | "bisac") && !code.is_empty()).then_some((authority, code))
                        })
                        .filter(|c| !self.checked_codes.contains(c))
                        .collect::<BTreeSet<_>>();
                    if codes.is_empty() {
                        self.advance();
                        continue;
                    }
                    self.pending_codes = codes.clone();
                    SubjectStep::ResolveCodes(codes.into_iter().map(|(authority, code)| SubjectCode { authority, code }).collect())
                }
                SubjectPhase::MetadataIsbns | SubjectPhase::PageIsbns | SubjectPhase::FinalIsbns => {
                    // Preserve page-reference priority, but do not lose an
                    // unqueried metadata ISBN if a page reference has no result.
                    let mut isbns = related_isbn::lookup_candidates(&metadata.identifiers);
                    isbns.extend(metadata.identifiers.iter().filter(|i| i.scheme() == &Scheme::Isbn).filter_map(|i| i.canonical_value().as_deref().and_then(from_metadata_value)));
                    let mut keys = BTreeSet::new();
                    isbns.retain(|isbn| {
                        let Some(key) = isbn_key(isbn) else { return false };
                        !self.checked_isbns.contains(&key) && keys.insert(key)
                    });
                    if isbns.is_empty() {
                        self.advance();
                        continue;
                    }
                    self.pending_isbns = keys;
                    SubjectStep::LookupIsbns(isbns)
                }
                SubjectPhase::Pages => {
                    self.pending_pages = true;
                    SubjectStep::InspectPages
                }
                SubjectPhase::TitleAuthor => SubjectStep::SearchTitleAuthor,
                SubjectPhase::Exhausted => return SubjectStep::Exhausted,
            };
            self.pending = true;
            return step;
        }
    }

    /// Call only after the requested step has finished successfully, including
    /// a definitive no-match/no-LCC result. On timeout or omitted results, retain
    /// the pending step for retry.
    pub fn finish_step(&mut self) {
        if !self.pending {
            return;
        }
        if self.pending_pages {
            self.pages_inspected = true;
            self.pending_pages = false;
        }
        self.checked_codes.append(&mut self.pending_codes);
        self.checked_isbns.append(&mut self.pending_isbns);
        self.pending = false;
        self.advance();
    }

    fn advance(&mut self) {
        self.stage = match self.stage {
            SubjectPhase::MetadataCodes => SubjectPhase::MetadataIsbns,
            SubjectPhase::MetadataIsbns => SubjectPhase::Pages,
            SubjectPhase::Pages => SubjectPhase::PageCodes,
            SubjectPhase::PageCodes => SubjectPhase::PageIsbns,
            SubjectPhase::PageIsbns => SubjectPhase::TitleAuthor,
            SubjectPhase::TitleAuthor => SubjectPhase::FinalIsbns,
            SubjectPhase::FinalIsbns | SubjectPhase::Exhausted => SubjectPhase::Exhausted,
        };
    }
}

pub fn isbn_key(value: &str) -> Option<String> {
    let isbn = from_metadata_value(value)?;
    if isbn.len() == 13 {
        return Some(isbn);
    }
    let mut isbn13 = format!("978{}", &isbn[..9]);
    let sum: u32 = isbn13.bytes().enumerate().map(|(i, c)| u32::from(c - b'0') * if i % 2 == 0 { 1 } else { 3 }).sum();
    isbn13.push(char::from(b'0' + ((10 - sum % 10) % 10) as u8));
    Some(isbn13)
}

#[cfg(test)]
mod tests {
    use super::*;
    use book_model::{BookSubject, Identifier, Scope};
    fn isbn(m: &mut BookMetadata, value: &str) {
        m.identifiers.push(Identifier::new(value, Scheme::Isbn, Scope::Book).unwrap());
    }
    fn code(m: &mut BookMetadata, authority: &str, value: &str) {
        m.subjects.push(BookSubject::new(None, value, "test", Some(authority.into()), Some(value.into())).unwrap());
    }
    #[test]
    fn fallback_waits_for_its_library_phase_and_resumes_at_a_coarse_checkpoint() {
        let mut metadata = BookMetadata::default();
        isbn(&mut metadata, "9781934356555");
        let mut pipeline = SubjectPipeline::default();
        assert!(pipeline.next_step_in_phase(&metadata, SubjectPhase::MetadataCodes).is_none());
        assert!(matches!(pipeline.next_step_in_phase(&metadata, SubjectPhase::MetadataIsbns), Some(SubjectStep::LookupIsbns(_))));
        pipeline.finish_step();
        assert!(pipeline.next_step_in_phase(&metadata, SubjectPhase::MetadataIsbns).is_none());
        let mut restored = SubjectPipeline::resume_at_phase(pipeline.phase());
        assert_eq!(restored.next_step_in_phase(&metadata, SubjectPhase::Pages), Some(SubjectStep::InspectPages));
        restored.finish_step();
        assert!(restored.next_step_in_phase(&metadata, SubjectPhase::Pages).is_none());
        assert_eq!(restored.phase(), SubjectPhase::PageCodes);
    }

    #[test]
    fn probing_a_later_phase_does_not_consume_unavailable_work() {
        let mut pipeline = SubjectPipeline::default();
        let metadata = BookMetadata::default();

        assert!(pipeline.next_step_in_phase(&metadata, SubjectPhase::MetadataCodes).is_none());
        assert_eq!(pipeline.next_step(&metadata), SubjectStep::InspectPages);
    }

    #[test]
    fn device_worker_limits_reserve_capacity_and_bound_memory() {
        for (cpus, workers) in [(0, 1), (1, 1), (2, 1), (4, 2), (8, 4), (64, 4)] {
            assert_eq!(crate::background_worker_limit(cpus), workers);
        }
    }
    #[test]
    fn bisac_never_stops_the_fallback_chain() {
        let mut m = BookMetadata::default();
        code(&mut m, "bisac", "COM051010");
        isbn(&mut m, "9781934356555");
        let mut p = SubjectPipeline::default();
        assert!(matches!(p.next_step(&m), SubjectStep::ResolveCodes(_)));
        p.finish_step();
        assert_eq!(p.next_step(&m), SubjectStep::LookupIsbns(vec!["9781934356555".into()]));
        p.finish_step(); // ISBN response has BISAC but no LCC.
        assert_eq!(p.next_step(&m), SubjectStep::InspectPages);
        code(&mut m, "bisac", "HIS000000");
        isbn(&mut m, "9780191507052");
        p.finish_step();
        assert!(matches!(p.next_step(&m), SubjectStep::ResolveCodes(_)));
        p.finish_step();
        assert_eq!(p.next_step(&m), SubjectStep::LookupIsbns(vec!["9780191507052".into()]));
        p.finish_step();
        assert_eq!(p.next_step(&m), SubjectStep::SearchTitleAuthor);
        p.finish_step();
        assert_eq!(p.next_step(&m), SubjectStep::Exhausted);
        assert_eq!(m.subjects.len(), 2); // Partial subject evidence is retained.
    }
    #[test]
    fn resolved_lcc_stops_but_unresolvable_lcc_does_not() {
        let mut m = BookMetadata::default();
        code(&mut m, "lcc", "NOT-A-CODE");
        let mut p = SubjectPipeline::default();
        assert!(matches!(p.next_step(&m), SubjectStep::ResolveCodes(_)));
        code(&mut m, "lcc", "Q125");
        assert_eq!(p.next_step(&m), SubjectStep::InspectPages); // ISBN still needed for authors.
        p.finish_step();
        assert_eq!(p.next_step(&m), SubjectStep::Complete);
    }
    #[test]
    fn coarse_resume_repeats_work_from_the_saved_phase() {
        let mut m = BookMetadata::default();
        isbn(&mut m, "1934356557");
        let mut p = SubjectPipeline::default();
        let request = p.next_step(&m);
        assert_eq!(request, SubjectStep::LookupIsbns(vec!["1934356557".into()]));
        assert_eq!(p.next_step(&m), request); // Timeout: no finish_step.
        p.finish_step();
        let mut p = SubjectPipeline::resume_at_phase(p.phase());
        assert_eq!(p.next_step(&m), SubjectStep::InspectPages);
        p.finish_step();
        // A restart forgets checked identifiers, so this phase may reconsider
        // an earlier ISBN. The durable provider-attempt ledger suppresses a
        // duplicate request in the enrichment host.
        assert_eq!(p.next_step(&m), SubjectStep::LookupIsbns(vec!["1934356557".into()]));
    }
}
