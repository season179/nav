package ui

import "charm.land/lipgloss/v2"

var (
	accent      = lipgloss.Color("63")
	accentDim   = lipgloss.Color("61")
	bgSubtle    = lipgloss.Color("235")
	textPrimary = lipgloss.Color("252")
	textMuted   = lipgloss.Color("244")
	textFaint   = lipgloss.Color("240")
	ok          = lipgloss.Color("42")
	warn        = lipgloss.Color("214")

	headerStyle = lipgloss.NewStyle().
			Foreground(textPrimary).
			Background(lipgloss.Color("236"))

	transcriptStyle = lipgloss.NewStyle().
			Padding(1, 2)

	sidebarStyle = lipgloss.NewStyle().
			Foreground(textMuted).
			Background(bgSubtle).
			Padding(1, 2)

	sidebarTitleStyle = lipgloss.NewStyle().
				Foreground(textPrimary).
				Bold(true)

	sidebarItemTitleStyle = lipgloss.NewStyle().
				Foreground(ok)

	sidebarItemBodyStyle = lipgloss.NewStyle().
				Foreground(textMuted)

	composerFrameStyle = lipgloss.NewStyle().
				Border(lipgloss.NormalBorder(), true, false, false, false).
				BorderForeground(accentDim).
				Padding(0, 2)

	composerTitleStyle = lipgloss.NewStyle().
				Foreground(textFaint)

	statusBarStyle = lipgloss.NewStyle().
			Foreground(textMuted).
			Background(lipgloss.Color("236"))

	roleSystemStyle = lipgloss.NewStyle().
			Foreground(textFaint)

	roleUserStyle = lipgloss.NewStyle().
			Foreground(lipgloss.Color("111")).
			Bold(true)

	roleAssistantStyle = lipgloss.NewStyle().
				Foreground(accent).
				Bold(true)

	roleToolStyle = lipgloss.NewStyle().
			Foreground(warn)

	messageBodyStyle = lipgloss.NewStyle().
				Foreground(textPrimary)

	thinkingBodyStyle = lipgloss.NewStyle().
				Foreground(textMuted).
				Background(bgSubtle).
				Padding(0, 1)
)
