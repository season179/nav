use crate::color::{blend, is_light};
use crate::terminal_palette::{best_color, default_bg};
use ratatui::style::Color;

pub(crate) const DEFAULT_COMPOSER_RGB: (u8, u8, u8) = (38, 38, 48);
pub(crate) const DEFAULT_POPUP_RGB: (u8, u8, u8) = (30, 30, 36);
/// Fixed composer surface used where palette probing is unavailable (e.g. tests).
pub(crate) const DEFAULT_COMPOSER_BG: Color = Color::Rgb(
    DEFAULT_COMPOSER_RGB.0,
    DEFAULT_COMPOSER_RGB.1,
    DEFAULT_COMPOSER_RGB.2,
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// Filled background colour for the composer block. Slightly lighter than a
    /// typical dark terminal background so the input area reads as distinct.
    pub composer_bg: Color,
    /// Filled background colour for popups. One step darker than the composer
    /// so overlays read as a layer above the input block.
    pub popup_bg: Color,
}

#[derive(Clone, Copy)]
enum Surface {
    Composer,
    Popup,
}

impl Surface {
    fn fallback_rgb(self) -> (u8, u8, u8) {
        match self {
            Self::Composer => DEFAULT_COMPOSER_RGB,
            Self::Popup => DEFAULT_POPUP_RGB,
        }
    }

    fn blend_on_terminal_bg(self, terminal_bg: (u8, u8, u8)) -> (u8, u8, u8) {
        let light = is_light(terminal_bg);
        let (overlay, alpha) = if light {
            match self {
                Self::Composer => ((0, 0, 0), 0.04),
                Self::Popup => ((0, 0, 0), 0.06),
            }
        } else {
            match self {
                Self::Composer => ((255, 255, 255), 0.15),
                Self::Popup => ((255, 255, 255), 0.12),
            }
        };
        blend(overlay, terminal_bg, alpha)
    }
}

impl Theme {
    pub fn from_extensions(
        requested: Option<&str>,
        extensions: &nav_core::ExtensionCatalog,
    ) -> Self {
        let terminal_bg = default_bg();
        let mut theme = Self::for_terminal_bg(terminal_bg);
        let Some(name) = requested.map(str::trim).filter(|name| !name.is_empty()) else {
            return theme;
        };
        if name == "default" {
            return theme;
        }
        let Some(extension_theme) = extensions.get_theme(name) else {
            eprintln!("nav-tui: theme `{name}` not found; using default");
            return theme;
        };
        if let Some(raw) = extension_theme.colors.composer_bg.as_deref() {
            set_surface_color(&mut theme.composer_bg, terminal_bg, Surface::Composer, raw);
        }
        if let Some(raw) = extension_theme.colors.popup_bg.as_deref() {
            set_surface_color(&mut theme.popup_bg, terminal_bg, Surface::Popup, raw);
        }
        theme
    }

    pub(crate) fn for_terminal_bg(terminal_bg: Option<(u8, u8, u8)>) -> Self {
        Self {
            composer_bg: surface_color(terminal_bg, Surface::Composer, None),
            popup_bg: surface_color(terminal_bg, Surface::Popup, None),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::for_terminal_bg(default_bg())
    }
}

fn set_surface_color(
    slot: &mut Color,
    terminal_bg: Option<(u8, u8, u8)>,
    surface: Surface,
    raw: &str,
) {
    if let Some(rgb) = parse_hex_rgb(raw) {
        *slot = surface_color(terminal_bg, surface, Some(rgb));
    }
}

fn surface_color(
    terminal_bg: Option<(u8, u8, u8)>,
    surface: Surface,
    override_rgb: Option<(u8, u8, u8)>,
) -> Color {
    best_color(surface_rgb(terminal_bg, surface, override_rgb))
}

fn surface_rgb(
    terminal_bg: Option<(u8, u8, u8)>,
    surface: Surface,
    override_rgb: Option<(u8, u8, u8)>,
) -> (u8, u8, u8) {
    if let Some(rgb) = override_rgb {
        if let Some(bg) = terminal_bg {
            return blend(rgb, bg, 0.85);
        }
        return rgb;
    }
    if let Some(bg) = terminal_bg {
        return surface.blend_on_terminal_bg(bg);
    }
    surface.fallback_rgb()
}

fn parse_hex_rgb(raw: &str) -> Option<(u8, u8, u8)> {
    let hex = raw.trim().strip_prefix('#').unwrap_or(raw.trim());
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        eprintln!("nav-tui: ignoring invalid theme color `{raw}`");
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some((r, g, b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nav_core::{ExtensionCatalog, ExtensionScope, ExtensionTheme, ThemeColors};

    #[test]
    fn default_theme_matches_existing_dark_surface_without_terminal_bg() {
        assert_eq!(surface_rgb(None, Surface::Composer, None), DEFAULT_COMPOSER_RGB);
        assert_eq!(surface_rgb(None, Surface::Popup, None), DEFAULT_POPUP_RGB);
    }

    #[test]
    fn dark_terminal_bg_lifts_composer_above_background() {
        assert_eq!(surface_rgb(Some((0, 0, 0)), Surface::Composer, None), (38, 38, 38));
        assert_eq!(surface_rgb(Some((0, 0, 0)), Surface::Popup, None), (30, 30, 30));
    }

    #[test]
    fn light_terminal_bg_darkens_surfaces() {
        assert_eq!(
            surface_rgb(Some((253, 246, 227)), Surface::Composer, None),
            (242, 236, 217)
        );
        assert_eq!(
            surface_rgb(Some((253, 246, 227)), Surface::Popup, None),
            (237, 231, 213)
        );
    }

    #[test]
    fn default_theme_colors_are_displayable() {
        let theme = Theme::default();
        assert_ne!(theme.composer_bg, Color::default());
        assert_ne!(theme.popup_bg, Color::default());
    }

    #[test]
    fn extension_theme_overrides_known_colors() {
        let catalog = ExtensionCatalog::new(
            Vec::new(),
            Vec::new(),
            vec![ExtensionTheme {
                name: "night".into(),
                description: None,
                colors: ThemeColors {
                    composer_bg: Some("#111827".into()),
                    popup_bg: Some("0f172a".into()),
                },
                extension_name: "demo".into(),
                scope: ExtensionScope::Project,
            }],
        );

        let theme = Theme::from_extensions(Some("night"), &catalog);
        assert_eq!(
            theme.composer_bg,
            surface_color(None, Surface::Composer, Some((17, 24, 39)))
        );
        assert_eq!(
            theme.popup_bg,
            surface_color(None, Surface::Popup, Some((15, 23, 42)))
        );
    }
}
