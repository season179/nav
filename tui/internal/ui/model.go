package ui

import (
	"context"
	"strings"

	"charm.land/bubbles/v2/key"
	"charm.land/bubbles/v2/textarea"
	tea "charm.land/bubbletea/v2"
	"nav.local/tui/internal/client"
)

type agentClient interface {
	Hello(context.Context) (client.Response, error)
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
	agent    agentClient
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

type agentReadyMsg struct {
	response client.Response
}

type agentErrorMsg struct {
	err error
}

func New(agent agentClient) Model {
	composer := newComposer()

	return Model{
		agent:    agent,
		composer: composer,
		status:   "connecting",
		messages: []transcriptItem{
			{Role: "system", Body: "nav rewrite: Rust coding-agent backend, Go Bubble Tea frontend."},
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

func (m Model) Init() tea.Cmd {
	return tea.Batch(m.composer.Focus(), func() tea.Msg {
		response, err := m.agent.Hello(context.Background())
		if err != nil {
			return agentErrorMsg{err: err}
		}
		return agentReadyMsg{response: response}
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
			return m, tea.Sequence(closeAgent(m.agent), tea.Quit)
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
	case agentReadyMsg:
		m.ready = true
		m.status = "ready"
		m.cwd = msg.response.CWD
		m.activity[0] = activityItem{Icon: "◇", Title: "backend", Body: msg.response.Name + " " + msg.response.Version}
		m.activity[1] = activityItem{Icon: "✓", Title: "cwd", Body: msg.response.CWD}
	case agentErrorMsg:
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

func closeAgent(agent agentClient) tea.Cmd {
	return func() tea.Msg {
		_ = agent.Close()
		return nil
	}
}
