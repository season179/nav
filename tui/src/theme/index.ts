/** Solarized Dark theme (Ethan Schoonover) — role-mapped for the nav TUI. */
export const theme = {
	accent: '#cb4b16', // orange — selection / brand highlight
	text: '#93a1a1', // base1 — body text (brightened from base0)
	inactive: '#839496', // base0 — comments, de-emphasized chrome (brightened from base01)
	subtle: '#073642', // base02 — very dim / panel background tone
	promptBorder: '#657b83', // base00 — divider lines (brightened from base01)
	userMessageBackground: '#073642', // base02 — highlighted message panel
	error: '#dc322f', // red
	success: '#859900', // green
} as const;
