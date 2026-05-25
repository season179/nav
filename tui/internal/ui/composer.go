package ui

import (
	"charm.land/bubbles/v2/textarea"
	"charm.land/lipgloss/v2"
)

func newComposer() textarea.Model {
	ta := textarea.New()
	ta.ShowLineNumbers = false
	ta.CharLimit = -1
	ta.DynamicHeight = true
	ta.MinHeight = textareaMinHeight
	ta.MaxHeight = textareaMaxHeight
	ta.Placeholder = "Ask nav to build, fix, explain, or inspect..."
	ta.Prompt = "::: "
	ta.SetVirtualCursor(false)
	ta.SetHeight(textareaMinHeight)
	ta.SetWidth(80)
	ta.SetStyles(composerStyles())
	ta.Focus()
	return ta
}

func composerStyles() textarea.Styles {
	styles := textarea.DefaultDarkStyles()
	styles.Focused.Base = lipgloss.NewStyle().Foreground(textPrimary)
	styles.Focused.Placeholder = lipgloss.NewStyle().Foreground(textMuted)
	styles.Focused.Prompt = lipgloss.NewStyle().Foreground(accent)
	styles.Focused.CursorLine = lipgloss.NewStyle()
	styles.Focused.Text = lipgloss.NewStyle().Foreground(textPrimary)
	styles.Blurred.Base = lipgloss.NewStyle().Foreground(textMuted)
	styles.Blurred.Placeholder = lipgloss.NewStyle().Foreground(textMuted)
	styles.Blurred.Prompt = lipgloss.NewStyle().Foreground(textMuted)
	return styles
}

func (m *Model) resizeComposer() {
	if m.width <= 0 {
		return
	}
	m.composer.SetWidth(max(20, m.width-6))
}
