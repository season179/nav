package ui

import (
	"context"
	"strings"

	"charm.land/bubbles/v2/key"
	"charm.land/bubbles/v2/textarea"
	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"
	"nav.local/tui/internal/backend"
)

const (
	textareaMinHeight = 3
	textareaMaxHeight = 10
	statusHeight      = 1
	headerHeight      = 1
)

type backendClient interface {
	Hello(context.Context) (backend.Response, error)
	Close() error
}

type transcriptItem struct {
	Role string
	Body string
}

type activityItem struct {
	Icon  string
	Title string
	Body  string
}

type Model struct {
	backend  backendClient
	composer textarea.Model
	width    int
	height   int
	ready    bool
	status   string
	cwd      string
	err      error
	messages []transcriptItem
	activity []activityItem
}

type backendReadyMsg struct {
	response backend.Response
}

type backendErrorMsg struct {
	err error
}

var (
	quitBinding    = key.NewBinding(key.WithKeys("ctrl+c", "esc"))
	sendBinding    = key.NewBinding(key.WithKeys("enter"))
	newlineBinding = key.NewBinding(key.WithKeys("ctrl+j"))
)

func New(client backendClient) Model {
	composer := newComposer()

	return Model{
		backend:  client,
		composer: composer,
		status:   "connecting",
		messages: []transcriptItem{
			{Role: "system", Body: "nav-2 fresh start: Rust coding-agent backend, Go Bubble Tea frontend."},
			{Role: "user", Body: "Create a new Rust project. The backend should be Rust, the TUI should be Go."},
			{Role: "assistant", Body: "Scaffolded a Rust workspace with a stdio backend and a user-facing nav command in Go."},
			{Role: "tool", Body: "make test passed for Cargo and Go modules."},
		},
		activity: []activityItem{
			{Icon: "◇", Title: "model", Body: "agent backend pending"},
			{Icon: "⋯", Title: "session", Body: "new workspace"},
			{Icon: "✓", Title: "tests", Body: "last run passed"},
		},
	}
}

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

func (m Model) Init() tea.Cmd {
	return tea.Batch(m.composer.Focus(), func() tea.Msg {
		response, err := m.backend.Hello(context.Background())
		if err != nil {
			return backendErrorMsg{err: err}
		}
		return backendReadyMsg{response: response}
	})
}

func (m Model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	var cmds []tea.Cmd

	switch msg := msg.(type) {
	case tea.WindowSizeMsg:
		m.width = msg.Width
		m.height = msg.Height
		m.resizeComposer()
	case tea.KeyMsg:
		switch {
		case key.Matches(msg, quitBinding):
			return m, tea.Sequence(closeBackend(m.backend), tea.Quit)
		case key.Matches(msg, newlineBinding):
			prevHeight := m.composer.Height()
			m.composer.InsertRune('\n')
			if prevHeight != m.composer.Height() {
				m.resizeComposer()
			}
			return m, nil
		case key.Matches(msg, sendBinding):
			return m.submitComposer()
		}
	case backendReadyMsg:
		m.ready = true
		m.status = "ready"
		m.cwd = msg.response.CWD
		m.activity[0] = activityItem{Icon: "◇", Title: "backend", Body: msg.response.Name + " " + msg.response.Version}
		m.activity[1] = activityItem{Icon: "✓", Title: "cwd", Body: msg.response.CWD}
	case backendErrorMsg:
		m.status = "backend error"
		m.err = msg.err
		m.activity[0] = activityItem{Icon: "×", Title: "backend", Body: msg.err.Error()}
	}

	nextComposer, cmd := m.composer.Update(msg)
	m.composer = nextComposer
	if cmd != nil {
		cmds = append(cmds, cmd)
	}
	return m, tea.Batch(cmds...)
}

func (m Model) submitComposer() (tea.Model, tea.Cmd) {
	value := m.composer.Value()
	if before, ok := strings.CutSuffix(value, "\\"); ok {
		m.composer.SetValue(before)
		m.composer.InsertRune('\n')
		return m, nil
	}

	value = strings.TrimSpace(value)
	if value == "" {
		return m, nil
	}

	m.messages = append(m.messages,
		transcriptItem{Role: "user", Body: value},
		transcriptItem{Role: "assistant", Body: "Hello from the Bubble Tea shell. The next step is wiring this send path into the Rust agent protocol."},
	)
	m.activity = append([]activityItem{{Icon: "⋯", Title: "last prompt", Body: value}}, m.activity...)
	if len(m.activity) > 5 {
		m.activity = m.activity[:5]
	}
	m.composer.Reset()
	m.resizeComposer()
	return m, nil
}

func (m *Model) resizeComposer() {
	if m.width <= 0 {
		return
	}
	m.composer.SetWidth(max(20, m.width-6))
}

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
		right = "nav-2"
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

func closeBackend(client backendClient) tea.Cmd {
	return func() tea.Msg {
		_ = client.Close()
		return nil
	}
}

func wrap(text string, width int) string {
	if width <= 1 {
		return text
	}

	words := strings.Fields(text)
	if len(words) == 0 {
		return ""
	}

	var lines []string
	line := words[0]
	for _, word := range words[1:] {
		if lipgloss.Width(line)+1+lipgloss.Width(word) > width {
			lines = append(lines, line)
			line = word
			continue
		}
		line += " " + word
	}
	lines = append(lines, line)
	return strings.Join(lines, "\n")
}

func tailLines(text string, height int) string {
	lines := strings.Split(text, "\n")
	if len(lines) > height {
		lines = lines[len(lines)-height:]
	}
	for len(lines) < height {
		lines = append(lines, "")
	}
	return strings.Join(lines, "\n")
}

func fitHeight(text string, height int) string {
	lines := strings.Split(text, "\n")
	if len(lines) > height {
		return strings.Join(lines[:height], "\n")
	}
	for len(lines) < height {
		lines = append(lines, "")
	}
	return strings.Join(lines, "\n")
}

func joinEdge(left, right string, width int) string {
	left = truncate(left, width)
	remaining := max(0, width-lipgloss.Width(left)-1)
	right = truncate(right, remaining)
	gap := max(0, width-lipgloss.Width(left)-lipgloss.Width(right))
	return left + strings.Repeat(" ", gap) + right
}

func barLine(left, right string, width int) string {
	if width <= 2 {
		return truncate(left, width)
	}
	return " " + joinEdge(left, right, width-2) + " "
}

func truncate(text string, width int) string {
	if width <= 0 {
		return ""
	}
	if lipgloss.Width(text) <= width {
		return text
	}

	runes := []rune(text)
	for lipgloss.Width(string(runes))+1 > width && len(runes) > 0 {
		runes = runes[:len(runes)-1]
	}
	return string(runes) + "…"
}

func shortPath(path string) string {
	if path == "" {
		return ""
	}
	parts := strings.Split(path, "/")
	if len(parts) <= 3 {
		return path
	}
	return strings.Join(parts[len(parts)-3:], "/")
}

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
)
