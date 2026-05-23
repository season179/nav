//! Shared formatting for duration and token counts in the TUI.

use std::time::Duration;

/// One decimal second when ≥1s, otherwise milliseconds.
pub(crate) fn format_elapsed(duration: Duration) -> String {
    let ms = duration.as_millis();
    if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
}

/// Format `tokens` as `<n.n>k` (one decimal). Caller must gate on `>= 1_000`.
pub(crate) fn format_tokens_k(tokens: u64) -> String {
    debug_assert!(tokens >= 1_000);
    let tenths = (tokens + 50) / 100;
    let whole = tenths / 10;
    let frac = tenths % 10;
    if frac == 0 {
        format!("{whole}k")
    } else {
        format!("{whole}.{frac}k")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_elapsed_subsecond_uses_ms() {
        assert_eq!(format_elapsed(Duration::from_millis(50)), "50ms");
    }

    #[test]
    fn format_elapsed_seconds_use_one_decimal() {
        assert_eq!(format_elapsed(Duration::from_millis(12_300)), "12.3s");
    }
}
