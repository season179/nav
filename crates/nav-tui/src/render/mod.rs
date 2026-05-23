//! Shared layout primitives ported from codex's `tui/src/render` module.
//!
//! Other parts of `nav-tui` historically did rect math inline. The
//! primitives in [`renderable`] (`ColumnRenderable`, `FlexRenderable`,
//! `RowRenderable`, `InsetRenderable`) let new cells and overlays compose
//! through a small, testable surface instead of growing more bespoke
//! `Layout::vertical([...]).areas(...)` call sites. Existing call sites
//! migrate organically as they're touched.

use ratatui::layout::Rect;

// Several primitives (`ColumnRenderable`, `RowRenderable`,
// `InsetRenderable`, `RenderableExt`) are intentionally pre-staged for the
// upcoming CELL-* / OVR-* ports referenced in LAY-01 (issue #174). They
// are exercised by unit tests but have no production caller yet, so the
// crate-default `dead_code` lint would otherwise flag them.
#[allow(dead_code)]
pub(crate) mod renderable;

/// Padding around a `Renderable`. Mirrors codex's `Insets` so the ports
/// translate one-to-one. Width 1 inset on each side trims a Rect by 2 in
/// the corresponding dimension.
#[allow(dead_code)] // pre-staged with the layout primitives; see renderable.rs note
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Insets {
    pub left: u16,
    pub top: u16,
    pub right: u16,
    pub bottom: u16,
}

#[allow(dead_code)]
impl Insets {
    /// Top, left, bottom, right (CSS order minus the comma confusion).
    pub fn tlbr(top: u16, left: u16, bottom: u16, right: u16) -> Self {
        Self {
            top,
            left,
            bottom,
            right,
        }
    }

    /// Symmetric vertical/horizontal padding.
    pub fn vh(v: u16, h: u16) -> Self {
        Self {
            top: v,
            left: h,
            bottom: v,
            right: h,
        }
    }
}

#[allow(dead_code)]
pub trait RectExt {
    fn inset(&self, insets: Insets) -> Rect;
}

impl RectExt for Rect {
    fn inset(&self, insets: Insets) -> Rect {
        let horizontal = insets.left.saturating_add(insets.right);
        let vertical = insets.top.saturating_add(insets.bottom);
        Rect {
            x: self.x.saturating_add(insets.left),
            y: self.y.saturating_add(insets.top),
            width: self.width.saturating_sub(horizontal),
            height: self.height.saturating_sub(vertical),
        }
    }
}
