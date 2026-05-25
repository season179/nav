package ui

import (
	"context"
	"errors"
	"strings"

	"charm.land/bubbles/v2/key"
	"charm.land/bubbles/v2/textarea"
	tea "charm.land/bubbletea/v2"
	"nav.local/tui/internal/client"
)

type agentClient interface {
	Connect(context.Context) (client.SessionInfo, error)
	SendMessage(context.Context, string) ([]client.Event, error)
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
	session client.SessionInfo
}

type agentEventsMsg struct {
	events []client.Event
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
			{Role: "assistant", Body: "Scaffolded a Rust workspace with a local HTTP/SSE backend and a user-facing nav command in Go."},
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
		session, err := m.agent.Connect(context.Background())
		if err != nil {
			return agentErrorMsg{err: err}
		}
		return agentReadyMsg{session: session}
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
		m.cwd = msg.session.CWD
		m.activity[0] = activityItem{Icon: "◇", Title: "backend", Body: msg.session.Endpoint}
		m.activity[1] = activityItem{Icon: "✓", Title: "session", Body: msg.session.SessionID}
	case agentEventsMsg:
		for _, event := range msg.events {
			m.applyAgentEvent(event)
		}
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
	)
	m.status = "thinking"
	m.err = nil
	m.prependActivity(activityItem{Icon: "⋯", Title: "last prompt", Body: value})
	m.composer.Reset()
	m.resizeComposer()
	return m, sendMessage(m.agent, value)
}

func closeAgent(agent agentClient) tea.Cmd {
	return func() tea.Msg {
		_ = agent.Close()
		return nil
	}
}

func sendMessage(agent agentClient, text string) tea.Cmd {
	return func() tea.Msg {
		events, err := agent.SendMessage(context.Background(), text)
		if err != nil {
			return agentErrorMsg{err: err}
		}
		return agentEventsMsg{events: events}
	}
}

func (m *Model) applyAgentEvent(event client.Event) {
	switch event.Type {
	case "run.started":
		m.status = "thinking"
	case "model.text_delta":
		m.appendAssistantDelta(event.Delta)
	case "message.delta":
		m.appendAssistantDelta(event.Text)
	case "run.completed", "message.completed":
		m.status = "ready"
	case "run.failed", "provider.error", "error":
		message := event.Message
		if message == "" {
			message = "backend run failed"
		}
		m.status = "backend error"
		m.err = errors.New(message)
		m.prependActivity(activityItem{Icon: "×", Title: event.Type, Body: message})
	}
}

func (m *Model) appendAssistantDelta(delta string) {
	if delta == "" {
		return
	}

	last := len(m.messages) - 1
	if last < 0 || m.messages[last].Role != "assistant" {
		m.messages = append(m.messages, transcriptItem{Role: "assistant", Body: delta})
		return
	}

	m.messages[last].Body += delta
}

func (m *Model) prependActivity(item activityItem) {
	m.activity = append([]activityItem{item}, m.activity...)
	if len(m.activity) > 5 {
		m.activity = m.activity[:5]
	}
}
