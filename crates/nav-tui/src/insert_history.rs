//! Insert finalized history rows into terminal scrollback above the inline viewport.
//! Ported from Codex's insert_history.rs with simplified wrapping.
//!
//! This module operates on raw stdout, not through ratatui's terminal. It
//! uses CSI scroll-region sequences to insert lines above the viewport.

use std::fmt;
use std::io::{self, Write};

use crossterm::cursor::{MoveDown, MoveTo, MoveToColumn, RestorePosition, SavePosition};
use crossterm::queue;
use crossterm::style::{
    Attribute as CAttribute, Color as CColor, Colors, Print, SetAttribute, SetBackgroundColor,
    SetColors, SetForegroundColor,
};
use crossterm::terminal::Clear;
use crossterm::terminal::ClearType;
use ratatui::style::{Color, Modifier};
use ratatui::text::{Line, Span};

/// Insert `lines` above the viewport using raw stdout operations.
///
/// `viewport_top` is the y-coordinate of the top row of the inline viewport.
/// `viewport_bottom` is the y-coordinate of the bottom row + 1.
/// `screen_height` is the total terminal height.
pub fn insert_history_lines(
    stdout: &mut impl Write,
    viewport_top: u16,
    viewport_bottom: u16,
    screen_height: u16,
    lines: Vec<Line<'static>>,
    wrap_width: u16,
) -> io::Result<()> {
    if lines.is_empty() {
        return Ok(());
    }

    let wrap_width = wrap_width.max(1) as usize;

    // Pre-wrap lines
    let mut wrapped: Vec<Line<'static>> = Vec::new();
    let mut wrapped_rows = 0usize;

    for line in &lines {
        let line_wrapped = simple_wrap_line(line, wrap_width);
        wrapped_rows += line_wrapped
            .iter()
            .map(|wl| wl.width().max(1).div_ceil(wrap_width))
            .sum::<usize>();
        wrapped.extend(line_wrapped);
    }
    let wrapped_lines = wrapped_rows as u16;
    if wrapped_lines == 0 {
        return Ok(());
    }

    let mut new_viewport_top = viewport_top;

    if viewport_bottom < screen_height {
        let scroll_amount = wrapped_lines.min(screen_height - viewport_bottom);
        let top_1based = viewport_top + 1;
        queue!(stdout, SetScrollRegion(top_1based..screen_height))?;
        queue!(stdout, MoveTo(0, viewport_top))?;
        for _ in 0..scroll_amount {
            queue!(stdout, Print("\x1bM"))?;
        }
        queue!(stdout, ResetScrollRegion)?;
        new_viewport_top = viewport_top + scroll_amount;
    }

    let cursor_top = new_viewport_top.saturating_sub(1);

    if cursor_top > 0 {
        queue!(stdout, SetScrollRegion(1..new_viewport_top))?;
        queue!(stdout, MoveTo(0, cursor_top))?;

        for line in &wrapped {
            queue!(stdout, Print("\r\n"))?;
            write_history_line(stdout, line, wrap_width)?;
        }

        queue!(stdout, ResetScrollRegion)?;
    }

    stdout.flush()?;
    Ok(())
}

/// Simple word-wrap: break a Line into multiple Lines that each fit within
/// `wrap_width` columns.
fn simple_wrap_line(line: &Line<'static>, wrap_width: usize) -> Vec<Line<'static>> {
    if wrap_width == 0 {
        return vec![line.clone()];
    }
    let line_width = line.width();
    if line_width <= wrap_width {
        return vec![line.clone()];
    }

    let mut words: Vec<(String, ratatui::style::Style)> = Vec::new();
    for span in &line.spans {
        let style = span.style.patch(line.style);
        for word in span.content.split_inclusive(' ') {
            if !word.is_empty() {
                words.push((word.to_string(), style));
            }
        }
    }

    if words.is_empty() {
        return vec![line.clone()];
    }

    let mut result = Vec::new();
    let mut current_spans: Vec<Span> = Vec::new();
    let mut current_width = 0;

    for (text, style) in words {
        let word_width = unicode_width::UnicodeWidthStr::width(text.as_str());
        if current_width > 0 && current_width + word_width > wrap_width {
            result.push(Line::from(std::mem::take(&mut current_spans)));
            current_width = 0;
        }
        current_width += word_width;
        current_spans.push(Span::styled(text, style));
    }

    if !current_spans.is_empty() {
        result.push(Line::from(current_spans));
    }

    if result.is_empty() {
        vec![line.clone()]
    } else {
        result
    }
}

fn write_history_line(writer: &mut impl Write, line: &Line, wrap_width: usize) -> io::Result<()> {
    let physical_rows = line.width().max(1).div_ceil(wrap_width.max(1)) as u16;
    if physical_rows > 1 {
        queue!(writer, SavePosition)?;
        for _ in 1..physical_rows {
            queue!(writer, MoveDown(1), MoveToColumn(0))?;
            queue!(writer, Clear(ClearType::UntilNewLine))?;
        }
        queue!(writer, RestorePosition)?;
    }
    let fg = ratatui_color_to_crossterm(line.style.fg.unwrap_or(Color::Reset));
    let bg = ratatui_color_to_crossterm(line.style.bg.unwrap_or(Color::Reset));
    queue!(writer, SetColors(Colors::new(fg, bg)))?;
    queue!(writer, Clear(ClearType::UntilNewLine))?;

    let merged_spans: Vec<Span> = line
        .spans
        .iter()
        .map(|s| Span {
            style: s.style.patch(line.style),
            content: s.content.clone(),
        })
        .collect();
    write_spans(writer, merged_spans.iter())
}

fn write_spans<'a, I>(mut writer: &mut impl Write, content: I) -> io::Result<()>
where
    I: IntoIterator<Item = &'a Span<'a>>,
{
    let mut fg = CColor::Reset;
    let mut bg = CColor::Reset;
    let mut last_modifier = Modifier::empty();
    for span in content {
        let mut modifier = Modifier::empty();
        modifier.insert(span.style.add_modifier);
        modifier.remove(span.style.sub_modifier);
        if modifier != last_modifier {
            let diff = ModifierDiff {
                from: last_modifier,
                to: modifier,
            };
            diff.queue(&mut writer)?;
            last_modifier = modifier;
        }
        let next_fg = ratatui_color_to_crossterm(span.style.fg.unwrap_or(Color::Reset));
        let next_bg = ratatui_color_to_crossterm(span.style.bg.unwrap_or(Color::Reset));
        if next_fg != fg || next_bg != bg {
            queue!(writer, SetColors(Colors::new(next_fg, next_bg)))?;
            fg = next_fg;
            bg = next_bg;
        }
        queue!(writer, Print(span.content.clone()))?;
    }

    queue!(
        writer,
        SetForegroundColor(CColor::Reset),
        SetBackgroundColor(CColor::Reset),
        SetAttribute(CAttribute::Reset),
    )
}

fn ratatui_color_to_crossterm(c: Color) -> CColor {
    match c {
        Color::Reset => CColor::Reset,
        Color::Black => CColor::Black,
        Color::Red => CColor::DarkRed,
        Color::Green => CColor::DarkGreen,
        Color::Yellow => CColor::DarkYellow,
        Color::Blue => CColor::DarkBlue,
        Color::Magenta => CColor::DarkMagenta,
        Color::Cyan => CColor::DarkCyan,
        Color::Gray => CColor::Grey,
        Color::DarkGray => CColor::DarkGrey,
        Color::LightRed => CColor::Red,
        Color::LightGreen => CColor::Green,
        Color::LightYellow => CColor::Yellow,
        Color::LightBlue => CColor::Blue,
        Color::LightMagenta => CColor::Magenta,
        Color::LightCyan => CColor::Cyan,
        Color::White => CColor::White,
        _ => CColor::Reset, // RGB/Indexed → Reset for now
    }
}

struct ModifierDiff {
    from: Modifier,
    to: Modifier,
}

impl ModifierDiff {
    fn queue<W: io::Write>(self, mut w: W) -> io::Result<()> {
        let removed = self.from - self.to;
        if removed.contains(Modifier::REVERSED) {
            queue!(w, SetAttribute(CAttribute::NoReverse))?;
        }
        if removed.contains(Modifier::BOLD) {
            queue!(w, SetAttribute(CAttribute::NormalIntensity))?;
        }
        if removed.contains(Modifier::ITALIC) {
            queue!(w, SetAttribute(CAttribute::NoItalic))?;
        }
        if removed.contains(Modifier::UNDERLINED) {
            queue!(w, SetAttribute(CAttribute::NoUnderline))?;
        }
        if removed.contains(Modifier::DIM) {
            queue!(w, SetAttribute(CAttribute::NormalIntensity))?;
        }
        let added = self.to - self.from;
        if added.contains(Modifier::REVERSED) {
            queue!(w, SetAttribute(CAttribute::Reverse))?;
        }
        if added.contains(Modifier::BOLD) {
            queue!(w, SetAttribute(CAttribute::Bold))?;
        }
        if added.contains(Modifier::ITALIC) {
            queue!(w, SetAttribute(CAttribute::Italic))?;
        }
        if added.contains(Modifier::UNDERLINED) {
            queue!(w, SetAttribute(CAttribute::Underlined))?;
        }
        if added.contains(Modifier::DIM) {
            queue!(w, SetAttribute(CAttribute::Dim))?;
        }
        Ok(())
    }
}

// ── ANSI scroll region commands ──────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetScrollRegion(pub std::ops::Range<u16>);

impl crossterm::Command for SetScrollRegion {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        write!(f, "\x1b[{};{}r", self.0.start, self.0.end)
    }
    #[cfg(windows)]
    fn execute_winapi(&self) -> std::io::Result<()> {
        Err(std::io::Error::other("use ANSI"))
    }
    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResetScrollRegion;

impl crossterm::Command for ResetScrollRegion {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        write!(f, "\x1b[r")
    }
    #[cfg(windows)]
    fn execute_winapi(&self) -> std::io::Result<()> {
        Err(std::io::Error::other("use ANSI"))
    }
    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        true
    }
}
