//! Folder component projection v1. These rules are part of replica behavior.
//! Character classes are explicit so compiler Unicode updates cannot change names.
use crate::filenames::{MAX_FILE_NAME_BYTES, is_reserved, truncate_utf8};

fn whitespace(c: char) -> bool {
    matches!(c, '\u{0009}'..='\u{000d}' | ' ' | '\u{0085}' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}')
}

fn forbidden(c: char) -> bool {
    matches!(c, '\u{0000}'..='\u{001f}' | '\u{007f}'..='\u{009f}' | '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
}

fn trim(value: &str) -> &str {
    value.trim_start_matches(whitespace).trim_end_matches(|c| c == '.' || whitespace(c))
}

/// A bounded, portable component derived from intent; callers retain the intent.
pub fn project_folder_name(requested: &str) -> String {
    candidate(requested, "")
}

pub(crate) fn numbered(requested: &str, number: usize) -> String {
    candidate(requested, &format!(" {number}"))
}

fn candidate(requested: &str, suffix: &str) -> String {
    let sanitized = requested.trim_matches(whitespace).chars().map(|c| if forbidden(c) { '_' } else { c }).collect::<String>();
    let capped = truncate_utf8(trim(&sanitized), MAX_FILE_NAME_BYTES - suffix.len());
    // Truncation can expose trailing dots/spaces or a reserved name.
    let base = trim(&capped);
    let base = if base.is_empty() || is_reserved(base) { "Folder" } else { base };
    format!("{base}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filenames::validate_component;

    #[test]
    fn unsafe_components_have_fixed_safe_projections() {
        for (input, expected) in [
            ("..", "Folder"),
            ("...", "Folder"),
            ("", "Folder"),
            ("\u{2003} . \u{2003}", "Folder"),
            ("CON", "Folder"),
            ("con.txt", "Folder"),
            ("LPT¹.log", "Folder"),
            ("A/B\\C:D", "A_B_C_D"),
            ("A\0B\u{0085}C", "A_B_C"),
            ("  Shelf. . ", "Shelf"),
            ("Étage", "Étage"),
            ("Shelf 2", "Shelf 2"),
        ] {
            assert_eq!(project_folder_name(input), expected, "input={input:?}");
            assert_eq!(validate_component(expected).unwrap(), expected);
            assert_eq!(project_folder_name(expected), expected, "projection must be idempotent");
        }
    }

    #[test]
    fn truncation_reserves_suffix_bytes_without_splitting_utf8() {
        for input in ["a".repeat(255), "界".repeat(100), "😀".repeat(100), format!("{} . trailing", "x".repeat(238))] {
            for number in [1, 2, 10, 100, usize::MAX] {
                let projected = if number == 1 { project_folder_name(&input) } else { numbered(&input, number) };
                assert!(projected.len() <= MAX_FILE_NAME_BYTES);
                assert_eq!(validate_component(&projected).unwrap(), projected);
                if number > 1 {
                    assert!(projected.ends_with(&format!(" {number}")));
                }
            }
        }
    }
}
