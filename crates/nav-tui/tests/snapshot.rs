use nav_core::{AgentEvent, TurnUsage};
use nav_tui::ChatWidget;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use serde_json::json;

fn buffer_to_text(buf: &Buffer) -> String {
    let area = buf.area();
    let mut out = String::with_capacity((area.width as usize + 1) * area.height as usize);
    for y in 0..area.height {
        let row_start = out.len();
        for x in 0..area.width {
            out.push_str(buf[(area.x + x, area.y + y)].symbol());
        }
        let trimmed = out[row_start..].trim_end_matches(' ').len();
        out.truncate(row_start + trimmed);
        out.push('\n');
    }
    out
}

#[test]
fn renders_full_turn_transcript() {
    let mut widget = ChatWidget::new();

    widget.push_user("list files in the repo");
    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "call_1".to_string(),
        name: "shell".to_string(),
        arguments: json!({ "command": ["ls"] }),
    });
    widget.ingest(AgentEvent::ToolCallOutput {
        call_id: "call_1".to_string(),
        output: "Cargo.toml\nsrc".to_string(),
        is_error: false,
    });
    widget.ingest(AgentEvent::AssistantMessageDone {
        text: "Two entries: Cargo.toml and src.".to_string(),
    });
    widget.ingest(AgentEvent::TurnComplete {
        usage: TurnUsage::default(),
    });

    let backend = TestBackend::new(60, 20);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| {
            let area = frame.area();
            frame.render_widget(&widget, area);
        })
        .expect("draw");

    let rendered = buffer_to_text(terminal.backend().buffer());
    insta::assert_snapshot!("full_turn_transcript", rendered);
}
