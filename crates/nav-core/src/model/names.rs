//! Small set of model-name heuristics shared by the CLI startup warning and
//! the transport-layer "did you mean…?" hint. The list is intentionally
//! short: not authoritative (provider catalogs drift faster than we ship),
//! just enough to catch the obvious typos like `gpt-55` or `o-3`.

/// Prefixes the typo guard considers plausibly-correct. Hitting any of these
/// suppresses the warning; missing all of them prints a one-line stderr note.
const KNOWN_PREFIXES: &[&str] = &["gpt-", "o1", "o3", "o4", "chatgpt-", "codex-", "computer-"];

/// Names suggested back to the user when the provider rejects an unknown
/// model. Keep this short — the hint is a starting point for fixing a typo,
/// not a directory of every available model.
const SUGGESTION_POOL: &[&str] = &[
    "gpt-5", "gpt-5.5", "gpt-5.1", "gpt-4.1", "gpt-4o", "o3", "o4-mini",
];

/// Returns true when `model` plausibly matches a known family prefix. Used
/// by the CLI to print a single-line warning *before* talking to the
/// provider — most "your model is wrong" failures happen at the 400 level
/// and a startup nudge is cheaper than a round-trip.
pub fn is_known_model_prefix(model: &str) -> bool {
    let normalized = model.to_ascii_lowercase();
    KNOWN_PREFIXES
        .iter()
        .any(|prefix| normalized.starts_with(prefix))
}

/// Pick up to `limit` names from [`SUGGESTION_POOL`] whose Levenshtein
/// distance to `model` is small enough to be worth showing. Returns an empty
/// vec when nothing is close — better silent than misleading.
pub fn suggest_models(model: &str, limit: usize) -> Vec<&'static str> {
    let model = model.trim().to_ascii_lowercase();
    if model.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<(&'static str, usize)> = SUGGESTION_POOL
        .iter()
        .map(|candidate| {
            (
                *candidate,
                edit_distance(&model, &candidate.to_ascii_lowercase()),
            )
        })
        .collect();
    scored.sort_by_key(|candidate| candidate.1);
    // Cap at edit distance 3 — beyond that the suggestions become noise.
    scored
        .into_iter()
        .take(limit)
        .take_while(|(_, dist)| *dist <= 3)
        .map(|(name, _)| name)
        .collect()
}

/// Format a "did you mean…?" suggestion list. Returns `None` when nothing
/// is close enough to be worth showing — callers decide how to splice it
/// into the surrounding sentence.
pub fn did_you_mean(model: &str) -> Option<String> {
    let suggestions = suggest_models(model, 3);
    match suggestions.as_slice() {
        [] => None,
        [one] => Some(format!("Did you mean `{one}`?")),
        [a, b] => Some(format!("Did you mean `{a}` or `{b}`?")),
        [a, b, c] => Some(format!("Did you mean `{a}`, `{b}`, or `{c}`?")),
        _ => None,
    }
}

/// Classic Levenshtein with substitution, insertion, and deletion all costing 1.
/// Plenty for typo-grade matching; we don't need a real Damerau extension.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let m = a.len();
    let n = b.len();
    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0usize; n + 1];
    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_prefixes_pass_typo_guard() {
        assert!(is_known_model_prefix("gpt-5.5"));
        assert!(is_known_model_prefix("gpt-4o"));
        assert!(is_known_model_prefix("o3"));
        assert!(is_known_model_prefix("o4-mini"));
        assert!(is_known_model_prefix("GPT-4"));
    }

    #[test]
    fn unknown_prefixes_flagged() {
        assert!(!is_known_model_prefix("claude-3-opus"));
        assert!(!is_known_model_prefix("gpt"));
        assert!(!is_known_model_prefix("llama"));
    }

    #[test]
    fn suggests_close_match() {
        let suggestions = suggest_models("gpt-55", 3);
        assert!(
            suggestions.contains(&"gpt-5.5"),
            "expected gpt-5.5 in {suggestions:?}"
        );
    }

    #[test]
    fn suggests_for_dropped_dash() {
        let suggestions = suggest_models("gpt4o", 3);
        assert!(
            suggestions.contains(&"gpt-4o"),
            "expected gpt-4o in {suggestions:?}"
        );
    }

    #[test]
    fn no_suggestions_when_distance_is_huge() {
        // Nothing in the pool is within 3 edits of this.
        let suggestions = suggest_models("totally-unrelated-name", 3);
        assert!(
            suggestions.is_empty(),
            "expected no suggestions for far-away name, got {suggestions:?}"
        );
    }

    #[test]
    fn did_you_mean_formats_for_zero_and_many() {
        assert_eq!(did_you_mean("totally-unrelated-name"), None);
        let one = did_you_mean("gpt-55").expect("expected suggestion");
        assert!(one.starts_with("Did you mean"));
        let multi = did_you_mean("gpt-4").expect("expected suggestions");
        assert!(multi.contains("Did you mean"));
        // No trailing whitespace.
        assert_eq!(multi.trim_end(), multi);
    }

    #[test]
    fn empty_model_returns_no_suggestions() {
        assert!(suggest_models("", 3).is_empty());
        assert!(suggest_models("   ", 3).is_empty());
    }
}
