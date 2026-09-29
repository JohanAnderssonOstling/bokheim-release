//! Chapter points from a CUE sheet. Audio remains in the discovered MP3 tracks.

use crate::AudiobookChapterMetadata;
use book_model::AudiobookTrack;

struct Point {
    file: String,
    title: Option<String>,
    offset_ms: u64,
}

fn argument(input: &str) -> Option<&str> {
    let input = input.trim();
    if let Some(quoted) = input.strip_prefix('"') {
        Some(quoted.split_once('"')?.0)
    } else {
        input.split_whitespace().next()
    }
}

fn timestamp(input: &str) -> Option<u64> {
    let mut fields = input.split(':');
    let minutes = fields.next()?.parse::<u64>().ok()?;
    let seconds = fields.next()?.parse::<u64>().ok()?;
    let frames = fields.next()?.parse::<u64>().ok()?;
    if fields.next().is_some() || seconds >= 60 || frames >= 75 { return None; }
    minutes.checked_mul(60)?.checked_add(seconds)?.checked_mul(1000)?.checked_add((frames * 1000 + 37) / 75)
}

fn track_for_file<'a>(tracks: &'a [AudiobookTrack], file: &str) -> Result<&'a AudiobookTrack, String> {
    let file = file.replace('\\', "/");
    if file.starts_with('/') || file.split('/').any(|part| part.is_empty() || part == "." || part == ".." || part.contains(':')) { return Err("unsafe CUE filename".into()); }
    let exact = tracks.iter().filter(|track| track.name.eq_ignore_ascii_case(&file)).collect::<Vec<_>>();
    let matches = if exact.is_empty() {
        tracks.iter().filter(|track| track.name.rsplit('/').next().is_some_and(|name| name.eq_ignore_ascii_case(&file))).collect::<Vec<_>>()
    } else { exact };
    if matches.len() != 1 { return Err(format!("CUE filename does not identify one MP3: {file}")); }
    Ok(matches[0])
}

pub(super) fn chapters(input: &str, tracks: &[AudiobookTrack]) -> Result<Vec<AudiobookChapterMetadata>, String> {
    let mut file = None::<String>;
    let mut current = None::<(Option<String>, Option<u64>)>;
    let mut points = Vec::<Point>::new();
    for line in input.lines() {
        let line = line.trim();
        let Some((command, rest)) = line.split_once(char::is_whitespace) else { continue };
        let rest = rest.trim();
        match command.to_ascii_uppercase().as_str() {
            "FILE" => {
                if let Some((title, offset_ms)) = current.take() {
                    points.push(Point { file: file.clone().ok_or("CUE track has no FILE")?, title, offset_ms: offset_ms.ok_or("CUE track has no INDEX 01")? });
                }
                let name = argument(rest).ok_or("CUE FILE has no filename")?;
                if !name.to_ascii_lowercase().ends_with(".mp3") { return Err("CUE references a non-MP3 file".into()); }
                file = Some(name.to_owned());
            }
            "TRACK" => {
                if let Some((title, offset_ms)) = current.take() {
                    points.push(Point { file: file.clone().ok_or("CUE track has no FILE")?, title, offset_ms: offset_ms.ok_or("CUE track has no INDEX 01")? });
                }
                if file.is_none() || !rest.split_whitespace().nth(1).is_some_and(|kind| kind.eq_ignore_ascii_case("AUDIO")) { return Err("CUE track is not audio".into()); }
                current = Some((None, None));
            }
            "TITLE" if current.is_some() => {
                let title = if rest.starts_with('"') { argument(rest).ok_or("CUE TITLE is empty")? } else { rest };
                current.as_mut().unwrap().0 = Some(title.to_owned());
            }
            "INDEX" if current.is_some() => {
                let mut fields = rest.split_whitespace();
                if fields.next() == Some("01") {
                    current.as_mut().unwrap().1 = Some(timestamp(fields.next().ok_or("missing CUE INDEX 01 position")?).ok_or("invalid CUE INDEX 01")?);
                }
            }
            _ => {},
        }
    }
    if let Some((title, offset_ms)) = current.take() {
        points.push(Point { file: file.ok_or("CUE track has no FILE")?, title, offset_ms: offset_ms.ok_or("CUE track has no INDEX 01")? });
    }
    if points.is_empty() { return Err("CUE has no INDEX 01 chapters".into()); }
    let mut starts = Vec::with_capacity(points.len());
    for (index, point) in points.into_iter().enumerate() {
        let track = track_for_file(tracks, &point.file)?;
        let start = track.start_ms.checked_add(point.offset_ms).ok_or("CUE position overflow")?;
        if start >= track.end_ms || starts.last().is_some_and(|(_, previous): &(String, u64)| *previous >= start) {
            return Err("CUE chapter positions are outside the audio or out of order".into());
        }
        starts.push((point.title.filter(|title| !title.trim().is_empty()).unwrap_or_else(|| format!("Chapter {}", index + 1)), start));
    }
    let duration = tracks.last().ok_or("audiobook has no MP3 tracks")?.end_ms;
    if starts.first().is_some_and(|(_, start)| *start > 0) {
        starts.insert(0, ("Opening".into(), 0));
    }
    Ok(starts.iter().enumerate().map(|(index, (title, start_ms))| AudiobookChapterMetadata {
        title: title.clone(), start_ms: *start_ms, end_ms: starts.get(index + 1).map_or(duration, |(_, next)| *next),
    }).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cue_can_split_one_file_and_continue_in_another() {
        let tracks = vec![
            AudiobookTrack { name: "Disc 1/01.mp3".into(), start_ms: 0, end_ms: 120_000, offset: 0, length: 1, archive_checksum: None },
            AudiobookTrack { name: "Disc 1/02.mp3".into(), start_ms: 120_000, end_ms: 180_000, offset: 1, length: 1, archive_checksum: None },
        ];
        let cue = "FILE \"01.mp3\" MP3\n  TRACK 01 AUDIO\n    TITLE \"One\"\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    TITLE \"Two\"\n    INDEX 01 01:00:00\nFILE \"02.mp3\" MP3\n  TRACK 03 AUDIO\n    TITLE \"Three\"\n    INDEX 01 00:00:00\n";
        let result = chapters(cue, &tracks).unwrap();
        assert_eq!(result.iter().map(|chapter| (chapter.title.as_str(), chapter.start_ms, chapter.end_ms)).collect::<Vec<_>>(), [("One", 0, 60_000), ("Two", 60_000, 120_000), ("Three", 120_000, 180_000)]);
        assert!(chapters("FILE \"missing.mp3\" MP3\nTRACK 01 AUDIO\nINDEX 01 00:00:00", &tracks).is_err());
        assert!(chapters("FILE \"01.mp3\" MP3\nTRACK 01 AUDIO\nINDEX 01 03:00:00", &tracks).is_err());
    }

    #[test]
    fn full_disc_paths_disambiguate_repeated_track_names() {
        let tracks = vec![
            AudiobookTrack { name: "Disc 2/01.mp3".into(), start_ms: 0, end_ms: 1000, offset: 0, length: 1, archive_checksum: None },
            AudiobookTrack { name: "Disc 1/01.mp3".into(), start_ms: 1000, end_ms: 2000, offset: 1, length: 1, archive_checksum: None },
        ];
        let cue = "FILE \"Disc 2/01.mp3\" MP3\nTRACK 01 AUDIO\nTITLE \"First\"\nINDEX 01 00:00:00\nFILE \"Disc 1/01.mp3\" MP3\nTRACK 02 AUDIO\nTITLE \"Second\"\nINDEX 01 00:00:00\n";
        let result = chapters(cue, &tracks).unwrap();
        assert_eq!(result.iter().map(|chapter| chapter.title.as_str()).collect::<Vec<_>>(), ["First", "Second"]);
        assert!(chapters("FILE \"01.mp3\" MP3\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n", &tracks).is_err());
    }
}
