/** Solarized Dark theme (Ethan Schoonover) — role-mapped for the nav TUI. */
export const theme = {
	accent: '#cb4b16', // orange — selection / brand highlight
	text: '#eee8d5', // base2 — body text (brightened from base1)
	inactive: '#93a1a1', // base1 — comments, de-emphasized chrome (brightened from base0)
	subtle: '#073642', // base02 — very dim / panel background tone
	promptBorder: '#839496', // base0 — divider lines (brightened from base00)
	userMessageBackground: '#073642', // base02 — highlighted message panel
	error: '#dc322f', // red
	success: '#859900', // green
	warning: '#b58900', // yellow
	info: '#2aa198', // cyan
} as const;
