use ratatui::style::Color;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// Filled background colour for the composer block. Slightly lighter than a
    /// typical dark terminal background so the input area reads as distinct.
    pub composer_bg: Color,
    /// Filled background colour for popups. One step darker than the composer
    /// so overlays read as a layer above the input block.
    pub popup_bg: Color,
}

impl Theme {
    pub fn from_extensions(
        requested: Option<&str>,
        extensions: &nav_core::ExtensionCatalog,
    ) -> Self {
        let mut theme = Self::default();
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
        if let Some(color) = extension_theme
            .colors
            .composer_bg
            .as_deref()
            .and_then(parse_hex_color)
        {
            theme.composer_bg = color;
        }
        if let Some(color) = extension_theme
            .colors
            .popup_bg
            .as_deref()
            .and_then(parse_hex_color)
        {
            theme.popup_bg = color;
        }
        theme
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            composer_bg: Color::Rgb(38, 38, 48),
            popup_bg: Color::Rgb(30, 30, 36),
        }
    }
}

fn parse_hex_color(raw: &str) -> Option<Color> {
    let hex = raw.trim().strip_prefix('#').unwrap_or(raw.trim());
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        eprintln!("nav-tui: ignoring invalid theme color `{raw}`");
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Color::Rgb(r, g, b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nav_core::{ExtensionCatalog, ExtensionScope, ExtensionTheme, ThemeColors};

    #[test]
    fn default_theme_matches_existing_dark_surface() {
        let theme = Theme::default();
        assert_eq!(theme.composer_bg, Color::Rgb(38, 38, 48));
        assert_eq!(theme.popup_bg, Color::Rgb(30, 30, 36));
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

        assert_eq!(theme.composer_bg, Color::Rgb(17, 24, 39));
        assert_eq!(theme.popup_bg, Color::Rgb(15, 23, 42));
    }
}
