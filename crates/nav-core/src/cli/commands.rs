//! Subcommands that hang off `nav`, such as `nav doctor`, `nav sessions`, and
//! `nav git`. The top-level [`super::Args`] struct owns the ordinary flags.

use clap::{Subcommand, ValueEnum};
use std::path::PathBuf;

use crate::context::ExportFormat;

#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
pub enum CliCommand {
    /// Export a stored session transcript.
    Export {
        /// Full session ULID or unique prefix.
        session_id: String,
        /// Output format. When omitted, inferred from --out extension and
        /// defaults to Markdown.
        #[arg(long, value_enum)]
        format: Option<CliExportFormat>,
        /// Output path. When omitted, the transcript is written to stdout.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// One-screen health check: runtime prerequisites, auth, storage,
    /// project context, and install state. Exit code is 1 when any check
    /// fails so it slots into CI / setup scripts.
    Doctor {
        /// Emit a single JSON object instead of the grouped text report.
        #[arg(long)]
        json: bool,
    },
    /// Advanced session workflows: fork, tree, labels, transcript search.
    Sessions {
        #[command(subcommand)]
        action: SessionsAction,
    },
    /// Git checkpointing helpers for reversible worktree states.
    Git {
        #[command(subcommand)]
        action: GitAction,
    },
    /// Inspect local extensions discovered from `.nav/extensions` and
    /// `~/.nav/extensions`.
    Extensions {
        #[command(subcommand)]
        action: ExtensionsAction,
    },
}

#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
pub enum ExtensionsAction {
    /// List discovered extension manifests and the surfaces they register.
    List,
}

#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
pub enum SessionsAction {
    /// Fork an existing session at a specific event seq (or "now" by default).
    Fork {
        /// Full session ULID or unique prefix to fork from.
        session_id: String,
        /// Event seq to fork at (inclusive). Omit to fork at the latest seq.
        #[arg(long)]
        at: Option<u64>,
        /// Display name for the new forked session.
        #[arg(long)]
        name: Option<String>,
    },
    /// Show the parent -> child tree rooted at this session.
    Tree {
        /// Full session ULID or unique prefix.
        session_id: String,
    },
    /// Attach a label to a session.
    Label {
        /// Full session ULID or unique prefix.
        session_id: String,
        /// Label text.
        label: String,
    },
    /// Detach a label from a session.
    Unlabel {
        /// Full session ULID or unique prefix.
        session_id: String,
        /// Label text.
        label: String,
    },
    /// Full-text search the persisted transcript across every session.
    Search {
        /// FTS5 MATCH expression (raw phrase or boolean).
        query: String,
        /// Maximum number of hits to return.
        #[arg(default_value_t = 20, long)]
        limit: usize,
        /// Restrict the search to sessions carrying this label.
        #[arg(long)]
        label: Option<String>,
    },
}

#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
pub enum GitAction {
    /// Save a checkpoint stash while keeping current files in place.
    Checkpoint {
        /// Optional label stored in the stash message.
        label: Vec<String>,
    },
    /// Stash current changes and leave the worktree clean.
    Stash {
        /// Optional label stored in the stash message.
        label: Vec<String>,
    },
    /// Apply a checkpoint/stash. Defaults to the newest nav checkpoint.
    Restore {
        /// Git stash ref, OID, or unique revision prefix.
        target: Option<String>,
    },
    /// List nav-created checkpoints and stashes.
    List,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum CliExportFormat {
    Md,
    Json,
}

impl From<CliExportFormat> for ExportFormat {
    fn from(value: CliExportFormat) -> Self {
        match value {
            CliExportFormat::Md => ExportFormat::Markdown,
            CliExportFormat::Json => ExportFormat::Json,
        }
    }
}
