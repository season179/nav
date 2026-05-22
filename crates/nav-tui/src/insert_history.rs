//! Insert finalized history rows into the terminal scrollback.
//!
//! Nav writes finalized cells above the inline viewport using escape
//! sequences; this is what gives the user native scrollback navigation, OS
//! text selection, and clipboard copy. Implementation derived from codex's
//! `insert_history.rs` (MIT) with simpler character-based wrap (no
//! URL-aware adaptive wrap) for now.
//!
//! See `docs/tui-architecture-migration.md` for the broader context.

use std::fmt;
use std::io;
use std::io::Write;

use crossterm::Command;
use crossterm::cursor::MoveDown;
use crossterm::cursor::MoveTo;
use crossterm::cursor::MoveToColumn;
use crossterm::cursor::RestorePosition;
use crossterm::cursor::SavePosition;
use crossterm::queue;
use crossterm::style::Color as CColor;
use crossterm::style::Colors;
use crossterm::style::Print;
use crossterm::style::SetAttribute;
use crossterm::style::SetBackgroundColor;
use crossterm::style::SetColors;
use crossterm::style::SetForegroundColor;
use crossterm::terminal::Clear;
use crossterm::terminal::ClearType;
use ratatui::layout::Size;
use ratatui::prelude::Backend;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::text::Line;

use crate::ansi::{queue_modifier_diff, ratatui_to_crossterm};
use ratatui::text::Span;

use crate::custom_terminal::Terminal;

/// Insert `lines` above the inline viewport, pushing them into the
/// terminal's native scrollback. Cursor position is left where it was.
///
/// `wrap_width` is the column width the caller used to render the lines —
/// must match the actual terminal width, since that's what natural terminal
/// wrap respects when the printed cells scroll up into scrollback. On the
/// first frame `terminal.viewport_area.width` is still `0` (set later by
/// `set_viewport_area` inside `draw_tui`), so callers must source the width
/// from `Backend::size()` rather than the viewport.
pub fn insert_history_lines<B>(
    terminal: &mut Terminal<B>,
    lines: Vec<Line<'static>>,
    wrap_width: u16,
) -> io::Result<()>
where
    B: Backend<Error = io::Error> + Write,
{
    let screen_size = terminal.backend().size().unwrap_or(Size::new(0, 0));
    let mut area = terminal.viewport_area;
    let mut should_update_area = false;
    let last_cursor_pos = terminal.last_known_cursor_pos;
    let writer = terminal.backend_mut();

    let wrap_width = wrap_width.max(1) as usize;
    let wrapped: Vec<Line<'static>> = lines
        .into_iter()
        .flat_map(|line| simple_wrap_line(line, wrap_width))
        .collect();
    let wrapped_rows = wrapped.len() as u16;

    let cursor_top = if area.bottom() < screen_size.height {
        // Viewport isn't at the bottom of the screen — scroll it down to make
        // room. Don't scroll past the bottom of the screen.
        let scroll_amount = wrapped_rows.min(screen_size.height - area.bottom());

        let top_1based = area.top() + 1;
        queue!(writer, SetScrollRegion(top_1based..screen_size.height))?;
        queue!(writer, MoveTo(0, area.top()))?;
        for _ in 0..scroll_amount {
            queue!(writer, Print("\x1bM"))?;
        }
        queue!(writer, ResetScrollRegion)?;

        let top = area.top().saturating_sub(1);
        area.y += scroll_amount;
        should_update_area = true;
        top
    } else {
        area.top().saturating_sub(1)
    };

    // Limit the scroll region to the lines from the top of the screen to the
    // top of the viewport. The cursor sits at the end of the region and new
    // lines push existing content up into scrollback.
    queue!(writer, SetScrollRegion(1..area.top()))?;
    queue!(writer, MoveTo(0, cursor_top))?;
    for line in &wrapped {
        queue!(writer, Print("\r\n"))?;
        write_history_line(writer, line, wrap_width)?;
    }
    queue!(writer, ResetScrollRegion)?;

    // Restore the cursor to where it was before insertion.
    queue!(writer, MoveTo(last_cursor_pos.x, last_cursor_pos.y))?;

    if should_update_area {
        terminal.set_viewport_area(area);
    }

    Ok(())
}

/// Naive grapheme-based wrap: split each line into chunks of at most
/// `wrap_width` columns, preserving span styles. Width comes from
/// `unicode-width` so CJK and emoji count as their actual cell width.
/// Word boundaries and URL grouping are not preserved; that's future work
/// to adopt codex's adaptive wrap.
fn simple_wrap_line(line: Line<'static>, wrap_width: usize) -> Vec<Line<'static>> {
    if wrap_width == 0 {
        return vec![line];
    }
    // Fast path: line already fits in the wrap budget. Avoids touching the
    // span Cow contents at all when the terminal is wide enough.
    if line.width() <= wrap_width {
        return vec![line];
    }

    use unicode_width::UnicodeWidthChar;
    let style = line.style;
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut current_width: usize = 0;
    let mut out: Vec<Line<'static>> = Vec::new();

    for span in line.spans.into_iter() {
        let content = span.content.into_owned();
        let span_style = span.style;
        let mut chunk = String::new();
        let mut chunk_width: usize = 0;
        for ch in content.chars() {
            let ch_width = ch.width().unwrap_or(0);
            if ch_width > 0
                && current_width + chunk_width + ch_width > wrap_width
                && (current_width + chunk_width) > 0
            {
                if !chunk.is_empty() {
                    current.push(Span::styled(std::mem::take(&mut chunk), span_style));
                    chunk_width = 0;
                }
                let mut line = Line::from(std::mem::take(&mut current));
                line = line.style(style);
                out.push(line);
                current_width = 0;
            }
            chunk.push(ch);
            chunk_width += ch_width;
        }
        if !chunk.is_empty() {
            current.push(Span::styled(chunk, span_style));
            current_width += chunk_width;
        }
    }

    if !current.is_empty() {
        let mut tail = Line::from(current);
        tail = tail.style(style);
        out.push(tail);
    } else if out.is_empty() {
        // Empty line input: preserve it as an empty line in output.
        out.push(Line::default().style(style));
    }
    out
}

fn write_history_line<W: Write>(writer: &mut W, line: &Line, wrap_width: usize) -> io::Result<()> {
    let physical_rows = line.width().max(1).div_ceil(wrap_width.max(1)) as u16;
    if physical_rows > 1 {
        queue!(writer, SavePosition)?;
        for _ in 1..physical_rows {
            queue!(writer, MoveDown(1), MoveToColumn(0))?;
            queue!(writer, Clear(ClearType::UntilNewLine))?;
        }
        queue!(writer, RestorePosition)?;
    }
    queue!(
        writer,
        SetColors(Colors::new(
            line.style
                .fg
                .map(ratatui_to_crossterm)
                .unwrap_or(CColor::Reset),
            line.style
                .bg
                .map(ratatui_to_crossterm)
                .unwrap_or(CColor::Reset),
        ))
    )?;
    queue!(writer, Clear(ClearType::UntilNewLine))?;
    let merged: Vec<Span> = line
        .spans
        .iter()
        .map(|s| Span {
            style: s.style.patch(line.style),
            content: s.content.clone(),
        })
        .collect();
    write_spans(writer, merged.iter())
}

fn write_spans<'a, I>(writer: &mut impl Write, content: I) -> io::Result<()>
where
    I: IntoIterator<Item = &'a Span<'a>>,
{
    let mut fg = Color::Reset;
    let mut bg = Color::Reset;
    let mut last_modifier = Modifier::empty();
    for span in content {
        let mut modifier = Modifier::empty();
        modifier.insert(span.style.add_modifier);
        modifier.remove(span.style.sub_modifier);
        if modifier != last_modifier {
            queue_modifier_diff(writer, last_modifier, modifier)?;
            last_modifier = modifier;
        }
        let next_fg = span.style.fg.unwrap_or(Color::Reset);
        let next_bg = span.style.bg.unwrap_or(Color::Reset);
        if next_fg != fg || next_bg != bg {
            queue!(
                writer,
                SetColors(Colors::new(
                    ratatui_to_crossterm(next_fg),
                    ratatui_to_crossterm(next_bg),
                ))
            )?;
            fg = next_fg;
            bg = next_bg;
        }
        queue!(writer, Print(span.content.clone()))?;
    }
    queue!(
        writer,
        SetForegroundColor(CColor::Reset),
        SetBackgroundColor(CColor::Reset),
        SetAttribute(crossterm::style::Attribute::Reset),
    )?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SetScrollRegion(std::ops::Range<u16>);

impl Command for SetScrollRegion {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        write!(f, "\x1b[{};{}r", self.0.start, self.0.end)
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> io::Result<()> {
        Err(io::Error::other(
            "tried to execute SetScrollRegion using WinAPI; use ANSI instead",
        ))
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResetScrollRegion;

impl Command for ResetScrollRegion {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        write!(f, "\x1b[r")
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> io::Result<()> {
        Err(io::Error::other(
            "tried to execute ResetScrollRegion using WinAPI; use ANSI instead",
        ))
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        true
    }
}
