//! Composable layout primitives ported from codex's `render/renderable.rs`.
//!
//! Everything in this file is intentionally kept structurally close to the
//! upstream codex implementation so future syncs stay trivial. The
//! semantics are:
//!
//! - [`Renderable`]: the only thing a primitive needs to know how to do is
//!   paint into a `Rect` and report how tall it wants to be at a given
//!   width. Cursor position and style flow up to the terminal driver so the
//!   caret lands at the right cell.
//! - [`ColumnRenderable`]: vertical stack, top-to-bottom, each child given
//!   `desired_height(width)` rows clipped to the parent area.
//! - [`FlexRenderable`]: column where children with `flex > 0` share the
//!   remaining space proportionally — like Flutter's `Flex` widget.
//! - [`RowRenderable`]: horizontal row of fixed-width children.
//! - [`InsetRenderable`]: padding wrapper that shrinks the child's rect
//!   by [`Insets`] on each side.

use std::sync::Arc;

use crossterm::cursor::SetCursorStyle;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::render::Insets;
use crate::render::RectExt as _;

pub trait Renderable {
    fn render(&self, area: Rect, buf: &mut Buffer);
    fn desired_height(&self, width: u16) -> u16;
    fn cursor_pos(&self, _area: Rect) -> Option<(u16, u16)> {
        None
    }
    fn cursor_style(&self, _area: Rect) -> SetCursorStyle {
        SetCursorStyle::DefaultUserShape
    }
}

/// Type-erased child slot. Borrowed children let callers compose without
/// taking ownership; owned children are boxed so heterogeneous types fit
/// the same `Vec`.
pub enum RenderableItem<'a> {
    Owned(Box<dyn Renderable + 'a>),
    Borrowed(&'a dyn Renderable),
}

impl<'a> Renderable for RenderableItem<'a> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        match self {
            RenderableItem::Owned(child) => child.render(area, buf),
            RenderableItem::Borrowed(child) => child.render(area, buf),
        }
    }

    fn desired_height(&self, width: u16) -> u16 {
        match self {
            RenderableItem::Owned(child) => child.desired_height(width),
            RenderableItem::Borrowed(child) => child.desired_height(width),
        }
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        match self {
            RenderableItem::Owned(child) => child.cursor_pos(area),
            RenderableItem::Borrowed(child) => child.cursor_pos(area),
        }
    }

    fn cursor_style(&self, area: Rect) -> SetCursorStyle {
        match self {
            RenderableItem::Owned(child) => child.cursor_style(area),
            RenderableItem::Borrowed(child) => child.cursor_style(area),
        }
    }
}

impl<'a> From<Box<dyn Renderable + 'a>> for RenderableItem<'a> {
    fn from(value: Box<dyn Renderable + 'a>) -> Self {
        RenderableItem::Owned(value)
    }
}

impl<'a, R> From<R> for Box<dyn Renderable + 'a>
where
    R: Renderable + 'a,
{
    fn from(value: R) -> Self {
        Box::new(value)
    }
}

impl Renderable for () {
    fn render(&self, _area: Rect, _buf: &mut Buffer) {}
    fn desired_height(&self, _width: u16) -> u16 {
        0
    }
}

// NOTE: codex's upstream `renderable.rs` also implements `Renderable` for
// `&str`, `String`, `Span`, `Line`, and `Paragraph`. Those impls lean on
// `ratatui::widgets::WidgetRef` and `Paragraph::line_count`, which are
// private or unstable in ratatui 0.30 (the version nav pins). Skipping
// them keeps the port clean against the current dependency — callers can
// wrap any widget in a thin `impl Renderable` if they need it. Re-add
// these blanket impls if ratatui re-exports the needed surface.

impl<R: Renderable> Renderable for Option<R> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if let Some(renderable) = self {
            renderable.render(area, buf);
        }
    }

    fn desired_height(&self, width: u16) -> u16 {
        if let Some(renderable) = self {
            renderable.desired_height(width)
        } else {
            0
        }
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        self.as_ref()
            .and_then(|renderable| renderable.cursor_pos(area))
    }

    fn cursor_style(&self, area: Rect) -> SetCursorStyle {
        self.as_ref()
            .map_or(SetCursorStyle::DefaultUserShape, |renderable| {
                renderable.cursor_style(area)
            })
    }
}

impl<R: Renderable> Renderable for Arc<R> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.as_ref().render(area, buf);
    }
    fn desired_height(&self, width: u16) -> u16 {
        self.as_ref().desired_height(width)
    }
    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        self.as_ref().cursor_pos(area)
    }
    fn cursor_style(&self, area: Rect) -> SetCursorStyle {
        self.as_ref().cursor_style(area)
    }
}

pub struct ColumnRenderable<'a> {
    children: Vec<RenderableItem<'a>>,
}

impl Renderable for ColumnRenderable<'_> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let mut y = area.y;
        for child in &self.children {
            let child_area = Rect::new(area.x, y, area.width, child.desired_height(area.width))
                .intersection(area);
            if !child_area.is_empty() {
                child.render(child_area, buf);
            }
            y += child_area.height;
        }
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.children
            .iter()
            .map(|child| child.desired_height(width))
            .sum()
    }

    /// Returns the cursor position of the first child that has a cursor
    /// position, offset by the child's position in the column. It is
    /// generally assumed that either zero or one child will have a cursor
    /// position.
    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        let mut y = area.y;
        for child in &self.children {
            let child_area = Rect::new(area.x, y, area.width, child.desired_height(area.width))
                .intersection(area);
            if !child_area.is_empty()
                && let Some((px, py)) = child.cursor_pos(child_area)
            {
                return Some((px, py));
            }
            y += child_area.height;
        }
        None
    }

    fn cursor_style(&self, area: Rect) -> SetCursorStyle {
        let mut y = area.y;
        for child in &self.children {
            let child_area = Rect::new(area.x, y, area.width, child.desired_height(area.width))
                .intersection(area);
            if !child_area.is_empty() && child.cursor_pos(child_area).is_some() {
                return child.cursor_style(child_area);
            }
            y += child_area.height;
        }
        SetCursorStyle::DefaultUserShape
    }
}

impl<'a> ColumnRenderable<'a> {
    pub fn new() -> Self {
        Self { children: vec![] }
    }

    pub fn with<I, T>(children: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<RenderableItem<'a>>,
    {
        Self {
            children: children.into_iter().map(Into::into).collect(),
        }
    }

    pub fn push(&mut self, child: impl Into<Box<dyn Renderable + 'a>>) {
        self.children.push(RenderableItem::Owned(child.into()));
    }
}

impl<'a> Default for ColumnRenderable<'a> {
    fn default() -> Self {
        Self::new()
    }
}

pub struct FlexChild<'a> {
    flex: i32,
    child: RenderableItem<'a>,
}

/// Lays out children in a column, with the ability to specify a flex
/// factor for each child.
///
/// Children with flex factor > 0 will be allocated the remaining space
/// after the non-flex children, proportional to the flex factor.
pub struct FlexRenderable<'a> {
    children: Vec<FlexChild<'a>>,
}

impl<'a> FlexRenderable<'a> {
    pub fn new() -> Self {
        Self { children: vec![] }
    }

    pub fn push(&mut self, flex: i32, child: impl Into<RenderableItem<'a>>) {
        self.children.push(FlexChild {
            flex,
            child: child.into(),
        });
    }

    /// Loosely inspired by Flutter's Flex widget.
    ///
    /// Ref https://github.com/flutter/flutter/blob/3fd81edbf1e015221e143c92b2664f4371bdc04a/packages/flutter/lib/src/rendering/flex.dart#L1205-L1209
    fn allocate(&self, area: Rect) -> Vec<Rect> {
        let mut allocated_rects = Vec::with_capacity(self.children.len());
        let mut child_sizes = vec![0; self.children.len()];
        let mut allocated_size = 0;
        let mut total_flex = 0;

        // 1. Allocate space to non-flex children.
        let max_size = area.height;
        let mut last_flex_child_idx = 0;
        for (i, FlexChild { flex, child }) in self.children.iter().enumerate() {
            if *flex > 0 {
                total_flex += flex;
                last_flex_child_idx = i;
            } else {
                child_sizes[i] = child
                    .desired_height(area.width)
                    .min(max_size.saturating_sub(allocated_size));
                allocated_size += child_sizes[i];
            }
        }
        let free_space = max_size.saturating_sub(allocated_size);
        // 2. Allocate space to flex children, proportional to their flex factor.
        let mut allocated_flex_space = 0;
        if total_flex > 0 {
            let space_per_flex = free_space / total_flex as u16;
            for (i, FlexChild { flex, child }) in self.children.iter().enumerate() {
                if *flex > 0 {
                    // Last flex child gets all the remaining space, to prevent a
                    // rounding error from leaving cells unassigned.
                    let max_child_extent = if i == last_flex_child_idx {
                        free_space - allocated_flex_space
                    } else {
                        space_per_flex * *flex as u16
                    };
                    let child_size = child.desired_height(area.width).min(max_child_extent);
                    child_sizes[i] = child_size;
                    allocated_flex_space += child_size;
                }
            }
        }

        let mut y = area.y;
        for size in child_sizes {
            let child_area = Rect::new(area.x, y, area.width, size);
            allocated_rects.push(child_area);
            y += child_area.height;
        }
        allocated_rects
    }
}

impl<'a> Renderable for FlexRenderable<'a> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.allocate(area)
            .into_iter()
            .zip(self.children.iter())
            .for_each(|(rect, child)| {
                child.child.render(rect, buf);
            });
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.allocate(Rect::new(0, 0, width, u16::MAX))
            .last()
            .map(|rect| rect.bottom())
            .unwrap_or(0)
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        self.allocate(area)
            .into_iter()
            .zip(self.children.iter())
            .find_map(|(rect, child)| child.child.cursor_pos(rect))
    }

    fn cursor_style(&self, area: Rect) -> SetCursorStyle {
        self.allocate(area)
            .into_iter()
            .zip(self.children.iter())
            .find_map(|(rect, child)| {
                child
                    .child
                    .cursor_pos(rect)
                    .map(|_| child.child.cursor_style(rect))
            })
            .unwrap_or(SetCursorStyle::DefaultUserShape)
    }
}

impl<'a> Default for FlexRenderable<'a> {
    fn default() -> Self {
        Self::new()
    }
}

pub struct RowRenderable<'a> {
    children: Vec<(u16, RenderableItem<'a>)>,
}

impl Renderable for RowRenderable<'_> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let mut x = area.x;
        for (width, child) in &self.children {
            let available_width = area.width.saturating_sub(x - area.x);
            let child_area = Rect::new(x, area.y, (*width).min(available_width), area.height);
            if child_area.is_empty() {
                break;
            }
            child.render(child_area, buf);
            x = x.saturating_add(*width);
        }
    }

    fn desired_height(&self, width: u16) -> u16 {
        let mut max_height = 0;
        let mut width_remaining = width;
        for (child_width, child) in &self.children {
            let w = (*child_width).min(width_remaining);
            if w == 0 {
                break;
            }
            let height = child.desired_height(w);
            if height > max_height {
                max_height = height;
            }
            width_remaining = width_remaining.saturating_sub(w);
        }
        max_height
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        let mut x = area.x;
        for (width, child) in &self.children {
            let available_width = area.width.saturating_sub(x - area.x);
            let child_area = Rect::new(x, area.y, (*width).min(available_width), area.height);
            if !child_area.is_empty()
                && let Some(pos) = child.cursor_pos(child_area)
            {
                return Some(pos);
            }
            x = x.saturating_add(*width);
        }
        None
    }

    fn cursor_style(&self, area: Rect) -> SetCursorStyle {
        let mut x = area.x;
        for (width, child) in &self.children {
            let available_width = area.width.saturating_sub(x - area.x);
            let child_area = Rect::new(x, area.y, (*width).min(available_width), area.height);
            if !child_area.is_empty() && child.cursor_pos(child_area).is_some() {
                return child.cursor_style(child_area);
            }
            x = x.saturating_add(*width);
        }
        SetCursorStyle::DefaultUserShape
    }
}

impl<'a> RowRenderable<'a> {
    pub fn new() -> Self {
        Self { children: vec![] }
    }

    pub fn push(&mut self, width: u16, child: impl Into<Box<dyn Renderable + 'a>>) {
        self.children
            .push((width, RenderableItem::Owned(child.into())));
    }
}

impl<'a> Default for RowRenderable<'a> {
    fn default() -> Self {
        Self::new()
    }
}

pub struct InsetRenderable<'a> {
    child: RenderableItem<'a>,
    insets: Insets,
}

impl<'a> Renderable for InsetRenderable<'a> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.child.render(area.inset(self.insets), buf);
    }
    fn desired_height(&self, width: u16) -> u16 {
        self.child
            .desired_height(width.saturating_sub(self.insets.left + self.insets.right))
            + self.insets.top
            + self.insets.bottom
    }
    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        self.child.cursor_pos(area.inset(self.insets))
    }

    fn cursor_style(&self, area: Rect) -> SetCursorStyle {
        self.child.cursor_style(area.inset(self.insets))
    }
}

impl<'a> InsetRenderable<'a> {
    pub fn new(child: impl Into<RenderableItem<'a>>, insets: Insets) -> Self {
        Self {
            child: child.into(),
            insets,
        }
    }
}

pub trait RenderableExt<'a> {
    fn inset(self, insets: Insets) -> RenderableItem<'a>;
}

impl<'a, R> RenderableExt<'a> for R
where
    R: Renderable + 'a,
{
    fn inset(self, insets: Insets) -> RenderableItem<'a> {
        let child: RenderableItem<'a> =
            RenderableItem::Owned(Box::new(self) as Box<dyn Renderable + 'a>);
        RenderableItem::Owned(Box::new(InsetRenderable { child, insets }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Style;

    /// Simple fixed-height stub. `marker` paints into every cell of the
    /// rect so tests can verify which child got which rows.
    struct Block {
        marker: char,
        height: u16,
    }

    impl Renderable for Block {
        fn render(&self, area: Rect, buf: &mut Buffer) {
            for y in area.y..area.bottom() {
                for x in area.x..area.right() {
                    buf[(x, y)].set_char(self.marker).set_style(Style::default());
                }
            }
        }
        fn desired_height(&self, _width: u16) -> u16 {
            self.height
        }
    }

    /// Stub that reports a cursor at the top-left of its rect.
    struct CursorBlock {
        marker: char,
        height: u16,
    }

    impl Renderable for CursorBlock {
        fn render(&self, area: Rect, buf: &mut Buffer) {
            for y in area.y..area.bottom() {
                for x in area.x..area.right() {
                    buf[(x, y)].set_char(self.marker);
                }
            }
        }
        fn desired_height(&self, _width: u16) -> u16 {
            self.height
        }
        fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
            Some((area.x, area.y))
        }
    }

    fn column_of(rows: &[char]) -> String {
        rows.iter().collect()
    }

    fn buf_column(buf: &Buffer, x: u16) -> String {
        (buf.area.y..buf.area.bottom())
            .map(|y| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
            .collect()
    }

    /// Convenience: wrap a stub child in `RenderableItem::Owned` so the
    /// `FlexRenderable::push` / `InsetRenderable::new` calls below stay
    /// short. Mirrors how production callers in codex wrap with
    /// `RenderableItem::Owned(Box::new(...))`.
    fn item<R: Renderable + 'static>(r: R) -> RenderableItem<'static> {
        RenderableItem::Owned(Box::new(r))
    }

    #[test]
    fn column_stacks_top_to_bottom() {
        let mut col = ColumnRenderable::new();
        col.push(Block { marker: 'a', height: 2 });
        col.push(Block { marker: 'b', height: 3 });

        let area = Rect::new(0, 0, 1, 5);
        let mut buf = Buffer::empty(area);
        col.render(area, &mut buf);

        assert_eq!(buf_column(&buf, 0), column_of(&['a', 'a', 'b', 'b', 'b']));
        assert_eq!(col.desired_height(1), 5);
    }

    #[test]
    fn column_clips_when_area_too_small() {
        let mut col = ColumnRenderable::new();
        col.push(Block { marker: 'a', height: 2 });
        col.push(Block { marker: 'b', height: 3 });

        // Only 3 rows of canvas — b should get truncated to 1 row.
        let area = Rect::new(0, 0, 1, 3);
        let mut buf = Buffer::empty(area);
        col.render(area, &mut buf);

        assert_eq!(buf_column(&buf, 0), column_of(&['a', 'a', 'b']));
    }

    #[test]
    fn column_cursor_pos_offsets_by_preceding_children() {
        let mut col = ColumnRenderable::new();
        col.push(Block { marker: 'a', height: 2 });
        col.push(CursorBlock { marker: 'c', height: 1 });

        let area = Rect::new(0, 0, 1, 3);
        assert_eq!(col.cursor_pos(area), Some((0, 2)));
    }

    #[test]
    fn flex_equal_split_two_children() {
        let mut flex = FlexRenderable::new();
        // Tall enough that the cap doesn't bind; flex math should choose
        // half the area for each child.
        flex.push(1, item(Block { marker: 'a', height: 100 }));
        flex.push(1, item(Block { marker: 'b', height: 100 }));

        let area = Rect::new(0, 0, 1, 6);
        let mut buf = Buffer::empty(area);
        flex.render(area, &mut buf);

        assert_eq!(buf_column(&buf, 0), column_of(&['a', 'a', 'a', 'b', 'b', 'b']));
    }

    #[test]
    fn flex_last_child_absorbs_rounding_remainder() {
        let mut flex = FlexRenderable::new();
        flex.push(1, item(Block { marker: 'a', height: 100 }));
        flex.push(1, item(Block { marker: 'b', height: 100 }));

        // 5 rows split 1:1 → 2 + 3 (last child gets the leftover row).
        let area = Rect::new(0, 0, 1, 5);
        let mut buf = Buffer::empty(area);
        flex.render(area, &mut buf);

        assert_eq!(buf_column(&buf, 0), column_of(&['a', 'a', 'b', 'b', 'b']));
    }

    #[test]
    fn flex_fixed_plus_flex_reserves_fixed_first() {
        let mut flex = FlexRenderable::new();
        flex.push(0, item(Block { marker: 'x', height: 2 })); // fixed
        flex.push(1, item(Block { marker: 'y', height: 100 })); // flex

        let area = Rect::new(0, 0, 1, 6);
        let mut buf = Buffer::empty(area);
        flex.render(area, &mut buf);

        assert_eq!(buf_column(&buf, 0), column_of(&['x', 'x', 'y', 'y', 'y', 'y']));
    }

    #[test]
    fn flex_child_capped_by_desired_height() {
        // Flex child wants only 2 rows even though it could have 5; the
        // remaining cells stay blank.
        let mut flex = FlexRenderable::new();
        flex.push(1, item(Block { marker: 'a', height: 2 }));

        let area = Rect::new(0, 0, 1, 5);
        let mut buf = Buffer::empty(area);
        flex.render(area, &mut buf);

        assert_eq!(buf_column(&buf, 0), column_of(&['a', 'a', ' ', ' ', ' ']));
    }

    #[test]
    fn row_lays_children_left_to_right() {
        let mut row = RowRenderable::new();
        row.push(2, Block { marker: 'a', height: 1 });
        row.push(3, Block { marker: 'b', height: 1 });

        let area = Rect::new(0, 0, 5, 1);
        let mut buf = Buffer::empty(area);
        row.render(area, &mut buf);

        let row_string: String = (0..5)
            .map(|x| buf[(x, 0)].symbol().chars().next().unwrap_or(' '))
            .collect();
        assert_eq!(row_string, "aabbb");
    }

    #[test]
    fn row_clips_overflowing_child() {
        let mut row = RowRenderable::new();
        row.push(3, Block { marker: 'a', height: 1 });
        row.push(5, Block { marker: 'b', height: 1 });

        // Only 4 cells of width — second child gets 1 cell.
        let area = Rect::new(0, 0, 4, 1);
        let mut buf = Buffer::empty(area);
        row.render(area, &mut buf);

        let row_string: String = (0..4)
            .map(|x| buf[(x, 0)].symbol().chars().next().unwrap_or(' '))
            .collect();
        assert_eq!(row_string, "aaab");
    }

    #[test]
    fn inset_shrinks_rect_on_every_side() {
        let area = Rect::new(0, 0, 6, 4);
        let inset = area.inset(Insets::tlbr(1, 2, 1, 2));
        assert_eq!(inset, Rect::new(2, 1, 2, 2));
    }

    #[test]
    fn inset_renderable_paints_inside_padding() {
        // 4x4 canvas, padding 1 on each side, child fills its 2x2 area.
        let area = Rect::new(0, 0, 4, 4);
        let mut buf = Buffer::empty(area);
        let inset = InsetRenderable::new(
            Box::new(Block { marker: '#', height: 100 }) as Box<dyn Renderable>,
            Insets::vh(1, 1),
        );
        inset.render(area, &mut buf);

        let mut rows = Vec::new();
        for y in 0..4 {
            let row: String = (0..4)
                .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
                .collect();
            rows.push(row);
        }
        assert_eq!(rows, vec!["    ", " ## ", " ## ", "    "]);
    }

    #[test]
    fn inset_desired_height_adds_vertical_padding() {
        let inset = InsetRenderable::new(
            Box::new(Block { marker: '_', height: 3 }) as Box<dyn Renderable>,
            Insets::tlbr(1, 0, 2, 0),
        );
        // 3 child rows + 1 top + 2 bottom = 6.
        assert_eq!(inset.desired_height(10), 6);
    }

    #[test]
    fn inset_each_side_independently() {
        // Asymmetric padding — verify each side trims the right edge.
        let area = Rect::new(0, 0, 6, 6);
        assert_eq!(
            area.inset(Insets::tlbr(1, 0, 0, 0)),
            Rect::new(0, 1, 6, 5),
            "top inset moves y down and shrinks height"
        );
        assert_eq!(
            area.inset(Insets::tlbr(0, 2, 0, 0)),
            Rect::new(2, 0, 4, 6),
            "left inset moves x right and shrinks width"
        );
        assert_eq!(
            area.inset(Insets::tlbr(0, 0, 3, 0)),
            Rect::new(0, 0, 6, 3),
            "bottom inset shrinks height only"
        );
        assert_eq!(
            area.inset(Insets::tlbr(0, 0, 0, 4)),
            Rect::new(0, 0, 2, 6),
            "right inset shrinks width only"
        );
    }
}
