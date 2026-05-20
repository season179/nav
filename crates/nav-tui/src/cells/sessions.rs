use nav_core::{SessionSummary, SessionTreeNode, TranscriptHit, layout_session_tree};
use ratatui::text::Line;

use crate::history::HistoryCell;

use super::row::{TranscriptRow, TranscriptRowKind};

pub struct SessionListCell {
    sessions: Vec<SessionSummary>,
}

impl SessionListCell {
    pub fn new(sessions: Vec<SessionSummary>) -> Self {
        Self { sessions }
    }
}

impl HistoryCell for SessionListCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let body = if self.sessions.is_empty() {
            "no stored sessions".to_string()
        } else {
            session_list_body(&self.sessions)
        };
        TranscriptRow::new(TranscriptRowKind::SessionList, body).render(width)
    }
}

fn session_list_body(sessions: &[SessionSummary]) -> String {
    let any_parent = sessions.iter().any(|s| s.parent_id.is_some());
    let layout: Vec<(usize, &SessionSummary)> = if any_parent {
        layout_session_tree(sessions)
    } else {
        sessions.iter().map(|s| (0usize, s)).collect()
    };
    let mut parts = Vec::new();
    for (depth, session) in layout {
        let indent = "  ".repeat(depth);
        let name = session.name.as_deref().unwrap_or("(unnamed)");
        let labels = labels_suffix(&session.labels);
        let title = session_title(session);
        let turn_word = if session.turn_count == 1 {
            "turn"
        } else {
            "turns"
        };
        parts.push(format!(
            "{indent}{}  {name}  created={}  active={}  {} {turn_word}{labels}",
            session.id, session.created_at, session.last_active, session.turn_count
        ));
        parts.push(format!("{indent}  {title}"));
    }
    parts.join("\n")
}

pub struct SessionNoticeCell {
    label: String,
    message: String,
}

impl SessionNoticeCell {
    pub fn new(label: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            message: message.into(),
        }
    }
}

impl HistoryCell for SessionNoticeCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        TranscriptRow::with_label(
            TranscriptRowKind::SessionNotice,
            self.label.as_str(),
            self.message.as_str(),
        )
        .render(width)
    }
}

pub struct SessionTreeCell {
    nodes: Vec<SessionTreeNode>,
}

impl SessionTreeCell {
    pub fn new(nodes: Vec<SessionTreeNode>) -> Self {
        Self { nodes }
    }
}

impl HistoryCell for SessionTreeCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let body = if self.nodes.is_empty() {
            "no descendants".to_string()
        } else {
            session_tree_body(&self.nodes)
        };
        TranscriptRow::new(TranscriptRowKind::SessionTree, body).render(width)
    }
}

fn session_tree_body(nodes: &[SessionTreeNode]) -> String {
    let mut lines = Vec::new();
    for node in nodes {
        let indent = "  ".repeat(node.depth as usize);
        let name = node.summary.name.as_deref().unwrap_or("(unnamed)");
        let labels = labels_suffix(&node.summary.labels);
        lines.push(format!(
            "{indent}{}  {name}  ({} turns){labels}",
            node.summary.id, node.summary.turn_count,
        ));
    }
    lines.join("\n")
}

fn labels_suffix(labels: &[String]) -> String {
    if labels.is_empty() {
        String::new()
    } else {
        format!(" [{}]", labels.join(","))
    }
}

fn session_title(session: &SessionSummary) -> &str {
    session
        .first_user_prompt
        .as_deref()
        .filter(|title| !title.trim().is_empty())
        .unwrap_or("(no prompt yet)")
}

pub struct TranscriptHitsCell {
    query: String,
    hits: Vec<TranscriptHit>,
}

impl TranscriptHitsCell {
    pub fn new(query: String, hits: Vec<TranscriptHit>) -> Self {
        Self { query, hits }
    }
}

impl HistoryCell for TranscriptHitsCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let body = if self.hits.is_empty() {
            format!("no matches for {:?}", self.query)
        } else {
            transcript_hits_body(&self.query, &self.hits)
        };
        TranscriptRow::new(TranscriptRowKind::TranscriptHits, body).render(width)
    }
}

fn transcript_hits_body(query: &str, hits: &[TranscriptHit]) -> String {
    let mut parts = vec![format!("matches for {query:?}:")];
    for hit in hits {
        let name = hit.summary.name.as_deref().unwrap_or("(unnamed)");
        parts.push(format!(
            "{}#{} [{}] {name}",
            &hit.session_id, hit.seq, hit.kind
        ));
        parts.push(format!("  {}", hit.snippet));
    }
    parts.join("\n")
}
