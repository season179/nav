package ui

import (
	"context"
	"errors"
	"fmt"
	"strings"

	"charm.land/bubbles/v2/key"
	"charm.land/bubbles/v2/textarea"
	tea "charm.land/bubbletea/v2"
	"nav.local/tui/internal/client"
	"nav.local/tui/internal/dialog"
	"nav.local/tui/internal/settings"
)

type agentClient interface {
	Connect(context.Context) (client.SessionInfo, error)
	StreamMessage(context.Context, string) (<-chan client.Event, <-chan error)
	Close() error
	ReloadSettings(context.Context) error
}

type transcriptItem struct {
	Role     string
	Body     string
	Thinking string
}

type activityItem struct {
	Icon  string
	Title string
	Body  string
}

type Model struct {
	agent        agentClient
	streamCancel context.CancelFunc
	composer     textarea.Model
	width        int
	height       int
	ready        bool
	status       string
	cwd          string
	err          error
	messages     []transcriptItem
	activity     []activityItem

	// Model selector dialog state.
	modelSelector *dialog.ModelSelector
	currentModel  string // "provider/model" or empty
}

type agentReadyMsg struct {
	session client.SessionInfo
}

type agentStream struct {
	events <-chan client.Event
	errs   <-chan error
}

type agentStreamStartedMsg struct {
	stream agentStream
}

type agentEventMsg struct {
	event  client.Event
	stream agentStream
}

type agentStreamDoneMsg struct{}

type agentErrorMsg struct {
	err error
}

type modelSelectedMsg struct {
	provider string
	model    string
	err      error
}

func New(agent agentClient) Model {
	composer := newComposer()

	m := Model{
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

	// Load current model from settings for display.
	if s, err := settings.Load(); err == nil && s.DefaultModel != nil {
		m.currentModel = s.DefaultModel.Provider + "/" + s.DefaultModel.Model
	}

	return m
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
		// When the model selector is active, route all key events to it.
		if m.modelSelector != nil && m.modelSelector.Active() {
			sel, cmd := m.modelSelector.HandleMsg(msg)
			if cmd != nil {
				cmds = append(cmds, cmd)
			}
			if sel != nil {
				return m.selectModel(sel.Provider, sel.Model)
			}
			if !m.modelSelector.Active() {
				// Dialog closed (esc or selection); refocus composer.
				cmds = append(cmds, m.composer.Focus())
			}
			return m, tea.Batch(cmds...)
		}

		switch {
		case key.Matches(msg, quitBinding):
			m.cancelActiveStream()
			return m, tea.Sequence(closeAgent(m.agent), tea.Quit)
		case key.Matches(msg, modelsBinding):
			return m.openModelSelector()
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
		m.setActivity(0, activityItem{Icon: "◇", Title: "backend", Body: msg.session.Endpoint})
		m.setActivity(1, activityItem{Icon: "✓", Title: "session", Body: msg.session.SessionID})

	case agentStreamStartedMsg:
		return m, waitForAgentEvent(msg.stream)

	case agentEventMsg:
		m.applyAgentEvent(msg.event)
		return m, waitForAgentEvent(msg.stream)

	case agentStreamDoneMsg:
		m.cancelActiveStream()
		if m.status == "thinking" {
			m.status = "ready"
		}

	case agentErrorMsg:
		m.cancelActiveStream()
		m.status = "backend error"
		m.err = msg.err
		m.setActivity(0, activityItem{Icon: "×", Title: "backend", Body: msg.err.Error()})

	case modelSelectedMsg:
		if msg.err != nil {
			m.status = "model error"
			m.err = msg.err
			m.prependActivity(activityItem{Icon: "×", Title: "model", Body: msg.err.Error()})
		} else {
			m.currentModel = msg.provider + "/" + msg.model
			m.status = "ready"
			m.prependActivity(activityItem{
				Icon:  "✓",
				Title: "model",
				Body:  m.currentModel,
			})
		}
	}

	nextComposer, cmd := m.composer.Update(msg)
	m.composer = nextComposer
	if cmd != nil {
		cmds = append(cmds, cmd)
	}
	return m, tea.Batch(cmds...)
}

func (m Model) openModelSelector() (tea.Model, tea.Cmd) {
	s, err := settings.Load()
	if err != nil {
		m.prependActivity(activityItem{Icon: "×", Title: "settings", Body: err.Error()})
		return m, nil
	}
	if len(s.Providers) == 0 {
		m.prependActivity(activityItem{Icon: "×", Title: "settings", Body: "no providers configured"})
		return m, nil
	}

	ms, err := dialog.NewModelSelector(s, m.width, m.height)
	if err != nil {
		m.prependActivity(activityItem{Icon: "×", Title: "settings", Body: err.Error()})
		return m, nil
	}
	m.modelSelector = ms
	return m, nil
}

func (m Model) selectModel(provider, model string) (tea.Model, tea.Cmd) {
	m.prependActivity(activityItem{Icon: "⋯", Title: "model", Body: "switching to " + provider + "/" + model})

	return m, func() tea.Msg {
		if err := settings.WriteDefaultModel(provider, model); err != nil {
			return modelSelectedMsg{err: fmt.Errorf("write settings: %w", err)}
		}
		// Reload backend settings so the new model takes effect immediately
		if err := m.agent.ReloadSettings(context.Background()); err != nil {
			return modelSelectedMsg{err: fmt.Errorf("reload settings: %w", err)}
		}
		return modelSelectedMsg{provider: provider, model: model}
	}
}

func (m Model) submitComposer() (tea.Model, tea.Cmd) {
	if m.streamCancel != nil {
		return m, nil
	}

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
	ctx, cancel := context.WithCancel(context.Background())
	m.streamCancel = cancel
	return m, sendMessage(m.agent, ctx, value)
}

func closeAgent(agent agentClient) tea.Cmd {
	return func() tea.Msg {
		_ = agent.Close()
		return nil
	}
}

func sendMessage(agent agentClient, ctx context.Context, text string) tea.Cmd {
	return func() tea.Msg {
		events, errs := agent.StreamMessage(ctx, text)
		return agentStreamStartedMsg{stream: agentStream{events: events, errs: errs}}
	}
}

func waitForAgentEvent(stream agentStream) tea.Cmd {
	return func() tea.Msg {
		event, ok := <-stream.events
		if ok {
			return agentEventMsg{event: event, stream: stream}
		}
		if err, ok := <-stream.errs; ok && err != nil {
			return agentErrorMsg{err: err}
		}
		return agentStreamDoneMsg{}
	}
}

func (m *Model) applyAgentEvent(event client.Event) {
	switch event.Type {
	case "run.started":
		m.status = "thinking"
	case "model.reasoning_delta":
		m.status = "thinking"
		m.appendAssistantReasoningDelta(event.Delta)
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
	default:
		title := event.Type
		if title == "" {
			title = "unknown event"
		}
		message := event.Message
		if message == "" {
			message = "unexpected backend event"
		}
		m.prependActivity(activityItem{Icon: "?", Title: title, Body: message})
	}
}

func (m *Model) cancelActiveStream() {
	if m.streamCancel == nil {
		return
	}
	m.streamCancel()
	m.streamCancel = nil
}

func (m *Model) appendAssistantDelta(delta string) {
	if delta == "" {
		return
	}

	assistant := m.currentAssistantMessage()
	assistant.Body += delta
}

func (m *Model) appendAssistantReasoningDelta(delta string) {
	if delta == "" {
		return
	}

	assistant := m.currentAssistantMessage()
	assistant.Thinking += delta
}

func (m *Model) currentAssistantMessage() *transcriptItem {
	last := len(m.messages) - 1
	if last < 0 || m.messages[last].Role != "assistant" {
		m.messages = append(m.messages, transcriptItem{Role: "assistant"})
		last = len(m.messages) - 1
	}

	return &m.messages[last]
}

func (m *Model) prependActivity(item activityItem) {
	m.activity = append([]activityItem{item}, m.activity...)
	if len(m.activity) > 5 {
		m.activity = m.activity[:5]
	}
}

func (m *Model) setActivity(index int, item activityItem) {
	for len(m.activity) <= index {
		m.activity = append(m.activity, activityItem{})
	}
	m.activity[index] = item
}
