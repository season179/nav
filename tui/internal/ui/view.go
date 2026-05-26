package ui

import (
	"strings"

	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"
)

const maxThinkingDisplayLines = 8

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

	content = fitHeight(content, m.height)

	// Overlay the model selector dialog if active.
	if m.modelSelector != nil && m.modelSelector.Active() {
		dialogView := m.modelSelector.View()
		if dialogView != "" {
			content = overlayCenter(content, dialogView)
		}
	}

	view := tea.NewView(content)
	view.AltScreen = true
	view.WindowTitle = "nav"
	return view
}

// overlayCenter places a dialog string centered on top of the
// background content, line-by-line.
func overlayCenter(bg, fg string) string {
	bgLines := strings.Split(bg, "\n")
	fgLines := strings.Split(fg, "\n")

	if len(fgLines) > len(bgLines) {
		return bg
	}

	startRow := (len(bgLines) - len(fgLines)) / 2
	for i, fgLine := range fgLines {
		row := startRow + i
		if row >= len(bgLines) {
			break
		}
		bgLine := bgLines[row]
		bgWidth := lipgloss.Width(bgLine)
		fgWidth := lipgloss.Width(fgLine)

		startCol := max(0, (bgWidth-fgWidth)/2)
		prefix := truncateToWidth(bgLine, startCol)
		prefixPad := startCol - lipgloss.Width(prefix)
		if prefixPad > 0 {
			prefix += strings.Repeat(" ", prefixPad)
		}

		afterCol := startCol + fgWidth
		suffix := sliceFromWidth(bgLine, afterCol)

		bgLines[row] = prefix + fgLine + suffix
	}

	return strings.Join(bgLines, "\n")
}

// truncateToWidth returns the prefix of s that fits within maxWidth.
func truncateToWidth(s string, maxWidth int) string {
	if maxWidth <= 0 {
		return ""
	}
	w := 0
	for i, r := range s {
		rw := runeWidth(r)
		if w+rw > maxWidth {
			return s[:i]
		}
		w += rw
	}
	return s
}

// sliceFromWidth returns the suffix of s starting at startCol.
func sliceFromWidth(s string, startCol int) string {
	w := 0
	for i, r := range s {
		if w >= startCol {
			return s[i:]
		}
		w += runeWidth(r)
	}
	return ""
}

func runeWidth(r rune) int {
	if r == '\t' {
		return 4 // approximate
	}
	// Simple heuristic: CJK chars are double-width.
	if r > 0x1100 &&
		(r <= 0x115f || r == 0x2329 || r == 0x232a ||
			(r >= 0x2e80 && r <= 0xa4cf && r != 0x303f) ||
			(r >= 0xac00 && r <= 0xd7a3) ||
			(r >= 0xf900 && r <= 0xfaff) ||
			(r >= 0xfe10 && r <= 0xfe19) ||
			(r >= 0xfe30 && r <= 0xfe6f) ||
			(r >= 0xff01 && r <= 0xff60) ||
			(r >= 0xffe0 && r <= 0xffe6) ||
			(r >= 0x20000 && r <= 0x2fffd) ||
			(r >= 0x30000 && r <= 0x3fffd)) {
		return 2
	}
	return 1
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

	hasThinking := item.Role == "assistant" && strings.TrimSpace(item.Thinking) != ""
	hasBody := strings.TrimSpace(item.Body) != ""

	sections := []string{label.Render(role)}
	if hasThinking {
		sections = append(sections, renderThinking(item.Thinking, width))
	}
	if hasBody || !hasThinking {
		body := messageBodyStyle.Width(width).Render(wrap(item.Body, width))
		sections = append(sections, body)
	}
	return lipgloss.JoinVertical(lipgloss.Left, sections...)
}

func renderThinking(thinking string, width int) string {
	content := strings.TrimSpace(thinking)
	if content == "" {
		return ""
	}

	content = lastLines(wrap(content, width), maxThinkingDisplayLines)
	return thinkingBodyStyle.Width(width).Render(content)
}

func lastLines(text string, maxLines int) string {
	if maxLines <= 0 {
		return ""
	}

	lines := strings.Split(text, "\n")
	if len(lines) > maxLines {
		lines = lines[len(lines)-maxLines:]
	}
	return strings.Join(lines, "\n")
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
	left := "enter send  ctrl+j newline  ctrl+m models  ctrl+c quit"
	right := "bubbletea"
	if m.currentModel != "" {
		right = m.currentModel
	}
	if m.ready && m.currentModel == "" {
		right = "backend connected"
	}
	if m.err != nil {
		right = "backend error"
	}
	return statusBarStyle.Width(m.width).Render(barLine(left, right, m.width))
}
