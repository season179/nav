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

fn render_widget(widget: &ChatWidget, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| {
            let area = frame.area();
            frame.render_widget(widget, area);
        })
        .expect("draw");
    buffer_to_text(terminal.backend().buffer())
}

fn widget_with_numbered_output(line_count: usize) -> ChatWidget {
    let mut widget = ChatWidget::new();
    widget.ingest(AgentEvent::ToolCallOutput {
        call_id: "call_1".to_string(),
        output: (0..line_count)
            .map(|i| format!("line {i:02}"))
            .collect::<Vec<_>>()
            .join("\n"),
        is_error: false,
    });
    widget
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

#[test]
fn overflowing_transcript_follows_newest_lines() {
    let widget = widget_with_numbered_output(20);

    let rendered = render_widget(&widget, 40, 6);

    assert!(!rendered.contains("line 00"), "{rendered}");
    assert!(rendered.contains("line 19"), "{rendered}");
}

#[test]
fn scroll_up_reveals_older_lines() {
    let mut widget = widget_with_numbered_output(20);

    widget.scroll_up(100, 40, 6);
    let rendered = render_widget(&widget, 40, 6);

    assert!(rendered.contains("line 00"), "{rendered}");
    assert!(!rendered.contains("line 19"), "{rendered}");
}

#[test]
fn scroll_down_returns_to_newest_lines() {
    let mut widget = widget_with_numbered_output(20);

    widget.scroll_up(100, 40, 6);
    widget.scroll_down(100, 40, 6);
    let rendered = render_widget(&widget, 40, 6);

    assert!(!rendered.contains("line 00"), "{rendered}");
    assert!(rendered.contains("line 19"), "{rendered}");
}

#[test]
fn scrolled_viewport_stays_stable_when_new_output_arrives() {
    let mut widget = widget_with_numbered_output(20);

    widget.scroll_up(5, 40, 6);
    let before = render_widget(&widget, 40, 6);
    widget.ingest(AgentEvent::ToolCallOutput {
        call_id: "call_2".to_string(),
        output: "new line".to_string(),
        is_error: false,
    });
    let after = render_widget(&widget, 40, 6);

    assert!(before.contains("line 10"), "{before}");
    assert!(after.contains("line 10"), "{after}");
    assert!(!after.contains("new line"), "{after}");
}

#[test]
fn scrolling_before_overflow_keeps_following_new_output() {
    let mut widget = widget_with_numbered_output(2);

    widget.scroll_up(5, 40, 6);
    widget.ingest(AgentEvent::ToolCallOutput {
        call_id: "call_2".to_string(),
        output: (2..20)
            .map(|i| format!("line {i:02}"))
            .collect::<Vec<_>>()
            .join("\n"),
        is_error: false,
    });
    let rendered = render_widget(&widget, 40, 6);

    assert!(!rendered.contains("line 00"), "{rendered}");
    assert!(rendered.contains("line 19"), "{rendered}");
}

#[test]
fn scroll_to_top_handles_transcripts_taller_than_u16() {
    let mut widget = widget_with_numbered_output(u16::MAX as usize + 5);

    widget.scroll_to_top(40, 6);
    let rendered = render_widget(&widget, 40, 6);

    assert!(rendered.contains("line 00"), "{rendered}");
    assert!(!rendered.contains("line 65539"), "{rendered}");
}
