pub(super) fn canonical_ddc_notation(value: &str) -> Option<String> {
    let compact = value.chars().filter(|character| !character.is_whitespace() && *character != '/').collect::<String>();
    if compact.len() == 3 && compact.bytes().all(|byte| byte.is_ascii_digit()) {
        return Some(compact);
    }
    let (whole, decimal) = compact.split_once('.')?;
    (whole.len() == 3 && whole.bytes().all(|byte| byte.is_ascii_digit()) && !decimal.is_empty() && decimal.bytes().all(|byte| byte.is_ascii_digit())).then(|| format!("{whole}.{decimal}"))
}

pub(super) fn specificity(notation: &str) -> usize {
    notation.split_once('.').map_or(0, |(_, decimal)| decimal.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalizes_complete_ddc_notations() {
        assert_eq!(canonical_ddc_notation(" 005.133 "), Some("005.133".to_owned()));
        assert_eq!(canonical_ddc_notation("005"), Some("005".to_owned()));
        assert_eq!(canonical_ddc_notation("5"), None);
    }
}
