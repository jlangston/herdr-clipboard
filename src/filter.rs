/// Case-insensitive subsequence match (fzf-style, no scoring):
/// every char of `needle` appears in `haystack` in order.
pub fn fuzzy_match(needle: &str, haystack: &str) -> bool {
    let mut hay = haystack.chars().flat_map(char::to_lowercase);
    needle
        .chars()
        .flat_map(char::to_lowercase)
        .all(|n| hay.any(|h| h == n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_needle_matches_everything() {
        assert!(fuzzy_match("", "anything"));
        assert!(fuzzy_match("", ""));
    }

    #[test]
    fn matches_case_insensitive_subsequence() {
        assert!(fuzzy_match("crgo", "Cargo.toml"));
        assert!(fuzzy_match("HELLO", "say hello world"));
    }

    #[test]
    fn rejects_out_of_order_or_missing_chars() {
        assert!(!fuzzy_match("ogr", "cargo"));
        assert!(!fuzzy_match("cargox", "cargo"));
        assert!(!fuzzy_match("oc", "cargo")); // 'o' after 'c' only
    }
}
