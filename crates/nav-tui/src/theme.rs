use ratatui::style::Color;

/// Filled background colour for the composer block. Slightly lighter than a
/// typical dark terminal background so the input area reads as a distinct rect.
pub const COMPOSER_BG: Color = Color::Rgb(38, 38, 48);

/// Filled background colour for the slash-command popup. One step darker than
/// `COMPOSER_BG` so the popup reads as a layer above the composer.
pub const POPUP_BG: Color = Color::Rgb(30, 30, 36);
