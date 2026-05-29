//! Validation for model-generated compaction summaries.
//!
//! A summary is only committed when it looks like a real summary: the required
//! template sections are present, the length is sane, and it does not contain
//! obvious model-error or refusal strings. Validation is pure so the orchestrator
//! can decide whether to commit without touching storage.

/// Template headings every committed summary must contain. Mirrors the template
/// in [`super::summary`].
pub const REQUIRED_SUMMARY_SECTIONS: &[&str] = &[
    "## Active Task",
    "## Goal",
    "## Constraints & Preferences",
    "## Completed Actions",
    "## Active State",
    "## In Progress",
    "## Blocked",
    "## Key Decisions",
];

/// Minimum number of content characters (template headings excluded) a summary
/// must carry. Guards against a model emitting only the empty section skeleton.
pub const MIN_SUMMARY_CONTENT_CHARS: usize = 40;

/// Upper bound on total summary length. The prompt caps generation at ~1.2k
/// tokens, so anything past this is a runaway response, not a summary.
pub const MAX_SUMMARY_CHARS: usize = 50_000;

/// High-signal phrases that mark a refusal or model error rather than a summary.
/// Matched case-insensitively. Kept to phrases that are extremely unlikely to
/// appear as legitimate quoted content in a summary, so a real summary that
/// merely *describes* a refusal is not falsely rejected (a false rejection
/// counts against the failure breaker).
pub const MODEL_ERROR_MARKERS: &[&str] = &[
    "as an ai language model",
    "i'm sorry, but as an ai",
    "i am sorry, but as an ai",
    "<|endoftext|>",
];

/// Why a candidate summary was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SummaryValidationError {
    /// The summary is empty or whitespace-only.
    Empty,
    /// Required template headings are missing.
    MissingSections(Vec<String>),
    /// The summary has the headings but not enough real content under them.
    TooShort { content_chars: usize, min: usize },
    /// The summary is implausibly long for a compaction summary.
    TooLong { chars: usize, max: usize },
    /// The summary contains a refusal or model-error marker.
    ModelErrorString(String),
}

impl std::fmt::Display for SummaryValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "summary is empty"),
            Self::MissingSections(sections) => {
                write!(f, "summary is missing sections: {}", sections.join(", "))
            }
            Self::TooShort { content_chars, min } => {
                write!(
                    f,
                    "summary has only {content_chars} content chars (min {min})"
                )
            }
            Self::TooLong { chars, max } => {
                write!(f, "summary is {chars} chars (max {max})")
            }
            Self::ModelErrorString(marker) => {
                write!(f, "summary contains a model-error marker: {marker:?}")
            }
        }
    }
}

impl std::error::Error for SummaryValidationError {}

/// Validate a candidate compaction summary before it is committed.
pub fn validate_compaction_summary(summary: &str) -> Result<(), SummaryValidationError> {
    if summary.trim().is_empty() {
        return Err(SummaryValidationError::Empty);
    }

    let total_chars = summary.chars().count();
    if total_chars > MAX_SUMMARY_CHARS {
        return Err(SummaryValidationError::TooLong {
            chars: total_chars,
            max: MAX_SUMMARY_CHARS,
        });
    }

    let lowercased = summary.to_lowercase();
    if let Some(marker) = MODEL_ERROR_MARKERS
        .iter()
        .find(|marker| lowercased.contains(**marker))
    {
        return Err(SummaryValidationError::ModelErrorString(
            (*marker).to_string(),
        ));
    }

    let missing: Vec<String> = REQUIRED_SUMMARY_SECTIONS
        .iter()
        .filter(|section| !summary.contains(**section))
        .map(|section| (*section).to_string())
        .collect();
    if !missing.is_empty() {
        return Err(SummaryValidationError::MissingSections(missing));
    }

    let content_chars = summary_content_chars(summary);
    if content_chars < MIN_SUMMARY_CONTENT_CHARS {
        return Err(SummaryValidationError::TooShort {
            content_chars,
            min: MIN_SUMMARY_CONTENT_CHARS,
        });
    }

    Ok(())
}

/// Count non-whitespace characters once the required template headings are
/// stripped, so a skeleton of empty sections reads as having no content.
fn summary_content_chars(summary: &str) -> usize {
    let mut content = summary.to_string();
    for section in REQUIRED_SUMMARY_SECTIONS {
        content = content.replace(section, "");
    }
    content.chars().filter(|c| !c.is_whitespace()).count()
}
