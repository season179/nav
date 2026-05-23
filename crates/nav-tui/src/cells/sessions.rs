use nav_core::{SessionTreeNode, TranscriptHit};
use ratatui::text::Line;

use crate::history::HistoryCell;

use super::row::{TranscriptRow, TranscriptRowKind};

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
