//! Transfer deadlines and progress notification policy.
use library_model::DownloadProgress;

pub const SMALL_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

// Allow a minute for setup plus transfer time at 64 KiB/s. A fixed short
// timeout would repeatedly abort large audiobooks even on a working link.
pub fn upload_timeout(bytes: u64) -> std::time::Duration {
    std::time::Duration::from_secs(60 + bytes.div_ceil(64 * 1024))
}

// Network chunk boundaries must not determine how often tabs redraw progress.
pub fn download_progress(completed: u64, total: Option<u64>, last: &mut web_time::Instant, now: web_time::Instant) -> Option<DownloadProgress> {
    let total = total.filter(|total| *total > 0)?;
    if completed < total && now.duration_since(*last) < std::time::Duration::from_millis(250) {
        return None;
    }
    *last = now;
    Some(DownloadProgress::from_ratio(completed, total))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use web_time::Instant;

    #[test]
    fn upload_deadline_scales_with_book_size() {
        assert_eq!(upload_timeout(0), Duration::from_secs(60));
        assert_eq!(upload_timeout(1), Duration::from_secs(61));
        assert_eq!(upload_timeout(64 * 1024), Duration::from_secs(61));
        assert_eq!(upload_timeout(1024 * 1024 * 1024), Duration::from_secs(60 + 16384));
    }

    #[test]
    fn rapid_chunks_only_emit_the_final_byte_count() {
        let now = Instant::now();
        let mut last = now;
        let updates = (1..=10_000).filter_map(|completed| download_progress(completed, Some(10_000), &mut last, now)).collect::<Vec<_>>();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].value(), 1.0);
    }

    #[test]
    fn sustained_downloads_report_periodically_and_flush_the_last_chunk() {
        let start = Instant::now();
        let mut last = start;
        let updates = (1..=999).filter_map(|completed| download_progress(completed, Some(999), &mut last, start + Duration::from_millis(completed)).map(|progress| (completed, progress))).collect::<Vec<_>>();
        assert_eq!(updates.iter().map(|(completed, _)| *completed).collect::<Vec<_>>(), [250, 500, 750, 999]);
        assert_eq!(updates.last().unwrap().1.value(), 1.0);
        assert!(download_progress(10, None, &mut last, start + Duration::from_secs(2)).is_none());
        assert!(download_progress(0, Some(0), &mut last, start + Duration::from_secs(2)).is_none());
    }
}
