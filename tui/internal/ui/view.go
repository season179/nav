package ui

import (
	"strings"

	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"
)

func (m Model) View() tea.View {
	if m.width == 0 || m.height == 0 {
		return tea.NewView("")
	}

	composer := m.renderComposer()
	composerHeight := lipgloss.Height(composer)
	mainHeight := max(1, m.height-headerHeight-statusHeight-composerHeight)

	content := lipgloss.JoinVertical(
		lipgloss.Left,
		m.renderHeader(),
		m.renderMain(mainHeight),
		composer,
		m.renderStatus(),
	)

	view := tea.NewView(fitHeight(content, m.height))
	view.AltScreen = true
	view.WindowTitle = "nav"
	return view
}

func (m Model) renderHeader() string {
	left := "nav"
	state := "Rust backend " + m.status
	if m.ready {
		state = "Rust backend ready"
	}
	if m.err != nil {
		state = "Rust backend unavailable"
	}
	right := shortPath(m.cwd)
	if right == "" {
		right = "nav"
	}

	line := barLine(left+"  "+state, right, m.width)
	return headerStyle.Width(m.width).Render(line)
}

func (m Model) renderMain(height int) string {
	if m.width >= 104 {
		sidebarWidth := 32
		transcriptWidth := max(30, m.width-sidebarWidth-1)
		transcript := m.renderTranscript(transcriptWidth, height)
		sidebar := m.renderActivity(sidebarWidth, height)
		return lipgloss.JoinHorizontal(lipgloss.Top, transcript, sidebar)
	}

	return m.renderTranscript(m.width, height)
}

func (m Model) renderTranscript(width, height int) string {
	bodyWidth := max(1, width-4)
	var parts []string
	for _, item := range m.messages {
		parts = append(parts, renderMessage(item, bodyWidth))
	}
	content := strings.Join(parts, "\n\n")
	content = tailLines(content, max(1, height-2))
	return transcriptStyle.Width(width).Height(height).Render(content)
}

func renderMessage(item transcriptItem, width int) string {
	role := strings.ToUpper(item.Role)
	var label lipgloss.Style
	switch item.Role {
	case "user":
		label = roleUserStyle
	case "assistant":
		label = roleAssistantStyle
	case "tool":
		label = roleToolStyle
	default:
		label = roleSystemStyle
	}

	body := messageBodyStyle.Width(width).Render(wrap(item.Body, width))
	return lipgloss.JoinVertical(lipgloss.Left, label.Render(role), body)
}

func (m Model) renderActivity(width, height int) string {
	bodyWidth := max(1, width-4)
	rows := []string{sidebarTitleStyle.Render("activity")}
	for _, item := range m.activity {
		title := sidebarItemTitleStyle.Render(item.Icon + " " + item.Title)
		body := sidebarItemBodyStyle.Width(bodyWidth).Render(wrap(item.Body, bodyWidth))
		rows = append(rows, lipgloss.JoinVertical(lipgloss.Left, title, body))
	}

	content := tailLines(strings.Join(rows, "\n\n"), max(1, height-2))
	return sidebarStyle.Width(width).Height(height).Render(content)
}

func (m Model) renderComposer() string {
	title := composerTitleStyle.Render("prompt")
	editor := m.composer.View()
	content := lipgloss.JoinVertical(lipgloss.Left, title, editor)
	return composerFrameStyle.Width(m.width).Render(content)
}

func (m Model) renderStatus() string {
	left := "enter send  ctrl+j newline  esc quit"
	right := "bubbletea"
	if m.ready {
		right = "backend connected"
	}
	if m.err != nil {
		right = "backend error"
	}
	return statusBarStyle.Width(m.width).Render(barLine(left, right, m.width))
}
