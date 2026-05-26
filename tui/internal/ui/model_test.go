package ui

import (
	"context"
	"testing"

	tea "charm.land/bubbletea/v2"
	"nav.local/tui/internal/client"
)

func TestSubmitComposerRendersAssistantDeltasFromBackendEvents(t *testing.T) {
	agent := &fakeAgent{
		events: []client.Event{
			{Type: "model.reasoning_delta", Delta: "thinking "},
			{Type: "model.text_delta", Delta: "hello "},
			{Type: "message.delta", Text: "from backend"},
			{Type: "run.completed"},
		},
	}
	model := New(agent)
	model.ready = true
	model.composer.SetValue("hello backend")

	next, cmd := model.submitComposer()
	if cmd == nil {
		t.Fatal("submitComposer returned nil command")
	}
	result := runUntilIdle(t, next, cmd)

	if agent.sentText != "hello backend" {
		t.Fatalf("sent text %q, want %q", agent.sentText, "hello backend")
	}
	last := result.messages[len(result.messages)-1]
	if last.Role != "assistant" || last.Body != "hello from backend" {
		t.Fatalf("last transcript item = %#v, want assistant backend response", last)
	}
	if last.Thinking != "thinking " {
		t.Fatalf("assistant thinking = %q, want streamed reasoning", last.Thinking)
	}
	if result.status != "ready" {
		t.Fatalf("status = %q, want ready", result.status)
	}
}

func TestSubmitComposerAppliesStreamedDeltaBeforeRunCompletes(t *testing.T) {
	events := make(chan client.Event, 2)
	errs := make(chan error, 1)
	agent := &fakeAgent{
		streamEvents: events,
		streamErrs:   errs,
	}
	model := New(agent)
	model.ready = true
	model.composer.SetValue("hello backend")

	next, cmd := model.submitComposer()
	if cmd == nil {
		t.Fatal("submitComposer returned nil command")
	}
	started := cmd()
	updated, waitForEvent := next.Update(started)
	if waitForEvent == nil {
		t.Fatalf("stream start produced no follow-up command after message %#v", started)
	}

	events <- client.Event{Type: "model.text_delta", Delta: "hello live"}
	streamed := waitForEvent()
	updated, waitForEvent = updated.Update(streamed)
	result := updated.(Model)

	last := result.messages[len(result.messages)-1]
	if last.Role != "assistant" || last.Body != "hello live" {
		t.Fatalf("last transcript item = %#v, want live assistant delta", last)
	}
	if result.status != "thinking" {
		t.Fatalf("status = %q, want thinking while run is still open", result.status)
	}
	if waitForEvent == nil {
		t.Fatal("stream should keep waiting after a non-terminal delta")
	}

	events <- client.Event{Type: "run.completed"}
	close(events)
	close(errs)
	completed := waitForEvent()
	updated, waitForEvent = updated.Update(completed)
	if waitForEvent == nil {
		t.Fatal("stream should wait for channel close after terminal event")
	}
	done := waitForEvent()
	updated, _ = updated.Update(done)
	result = updated.(Model)
	if result.status != "ready" {
		t.Fatalf("status = %q, want ready after run.completed", result.status)
	}
	if result.streamCancel != nil {
		t.Fatal("stream cancel was not cleared after stream completion")
	}
}

func TestQuitCancelsActiveStreamBeforeClosingAgent(t *testing.T) {
	agent := &fakeAgent{
		streamEvents: make(chan client.Event),
		streamErrs:   make(chan error),
	}
	model := New(agent)
	model.ready = true
	model.composer.SetValue("hello backend")

	next, cmd := model.submitComposer()
	if cmd == nil {
		t.Fatal("submitComposer returned nil command")
	}
	started := cmd()
	updated, waitForEvent := next.Update(started)
	if waitForEvent == nil {
		t.Fatalf("stream start produced no follow-up command after message %#v", started)
	}
	if agent.streamCtx == nil {
		t.Fatal("fake agent did not capture stream context")
	}

	updated, _ = updated.Update(tea.KeyPressMsg{Code: tea.KeyEsc})

	if err := agent.streamCtx.Err(); err != context.Canceled {
		t.Fatalf("stream context error = %v, want context.Canceled", err)
	}
	if updated.(Model).streamCancel != nil {
		t.Fatal("stream cancel was not cleared after quit")
	}
}

func TestSubmitComposerIgnoresSecondPromptWhileStreamIsActive(t *testing.T) {
	agent := &fakeAgent{
		streamEvents: make(chan client.Event),
		streamErrs:   make(chan error),
	}
	model := New(agent)
	model.ready = true
	model.composer.SetValue("first prompt")

	next, cmd := model.submitComposer()
	if cmd == nil {
		t.Fatal("first submitComposer returned nil command")
	}
	started := cmd()
	updated, waitForEvent := next.Update(started)
	if waitForEvent == nil {
		t.Fatalf("stream start produced no follow-up command after message %#v", started)
	}

	active := updated.(Model)
	active.composer.SetValue("second prompt")
	next, cmd = active.submitComposer()

	result := next.(Model)
	if cmd != nil {
		t.Fatal("second submitComposer returned a command while stream was active")
	}
	if len(result.messages) != len(active.messages) {
		t.Fatalf("message count = %d, want %d", len(result.messages), len(active.messages))
	}
	if got := result.composer.Value(); got != "second prompt" {
		t.Fatalf("composer value = %q, want second prompt preserved", got)
	}
}

func TestSubmitComposerSurfacesBackendRunFailures(t *testing.T) {
	agent := &fakeAgent{
		events: []client.Event{
			{Type: "run.failed", Message: "MissingApiKey: OPENAI_API_KEY is not set"},
		},
	}
	model := New(agent)
	model.ready = true
	model.composer.SetValue("hello backend")

	next, cmd := model.submitComposer()
	if cmd == nil {
		t.Fatal("submitComposer returned nil command")
	}
	result := runUntilIdle(t, next, cmd)

	if result.err == nil {
		t.Fatal("expected backend error to be visible")
	}
	if result.status != "backend error" {
		t.Fatalf("status = %q, want backend error", result.status)
	}
	if got := result.activity[0].Body; got != "MissingApiKey: OPENAI_API_KEY is not set" {
		t.Fatalf("activity error = %q, want backend failure message", got)
	}
}

func TestAgentReadyKeepsShortActivityListSafe(t *testing.T) {
	model := New(&fakeAgent{})
	model.activity = nil

	updated, _ := model.Update(agentReadyMsg{session: client.SessionInfo{
		SessionID: "session-1",
		Endpoint:  "http://backend.test",
		CWD:       "/tmp/nav",
	}})
	result := updated.(Model)

	if len(result.activity) < 2 {
		t.Fatalf("activity length = %d, want at least 2", len(result.activity))
	}
	if got := result.activity[0].Body; got != "http://backend.test" {
		t.Fatalf("backend activity = %q, want endpoint", got)
	}
	if got := result.activity[1].Body; got != "session-1" {
		t.Fatalf("session activity = %q, want session id", got)
	}
}

func TestApplyAgentEventSurfacesUnknownEvents(t *testing.T) {
	model := New(&fakeAgent{})

	model.applyAgentEvent(client.Event{Type: "tool.call.started", Message: "running shell"})

	if got := model.activity[0]; got.Icon != "?" || got.Title != "tool.call.started" || got.Body != "running shell" {
		t.Fatalf("unknown event activity = %#v", got)
	}
}

func TestApplyAgentEventAcceptsReasoningDeltasWithoutActivityNoise(t *testing.T) {
	model := New(&fakeAgent{})
	activityCount := len(model.activity)

	model.applyAgentEvent(client.Event{Type: "model.reasoning_delta", Delta: "thinking"})

	if len(model.activity) != activityCount {
		t.Fatalf("activity length = %d, want %d", len(model.activity), activityCount)
	}
	if model.status != "thinking" {
		t.Fatalf("status = %q, want thinking", model.status)
	}
	last := model.messages[len(model.messages)-1]
	if last.Role != "assistant" || last.Thinking != "thinking" {
		t.Fatalf("last transcript item = %#v, want assistant reasoning", last)
	}
}

type fakeAgent struct {
	sentText     string
	streamCtx    context.Context
	events       []client.Event
	streamEvents <-chan client.Event
	streamErrs   <-chan error
}

func (a *fakeAgent) Connect(context.Context) (client.SessionInfo, error) {
	return client.SessionInfo{SessionID: "session-1", Endpoint: "http://backend.test", CWD: "/tmp/nav"}, nil
}

func (a *fakeAgent) StreamMessage(ctx context.Context, text string) (<-chan client.Event, <-chan error) {
	a.streamCtx = ctx
	a.sentText = text
	if a.streamEvents != nil || a.streamErrs != nil {
		return a.streamEvents, a.streamErrs
	}

	events := make(chan client.Event, len(a.events))
	for _, event := range a.events {
		events <- event
	}
	close(events)

	errs := make(chan error, 1)
	close(errs)
	return events, errs
}

func (a *fakeAgent) Close() error {
	return nil
}

func runUntilIdle(t *testing.T, model tea.Model, cmd tea.Cmd) Model {
	t.Helper()
	for cmd != nil {
		var message tea.Msg
		message = cmd()
		model, cmd = model.Update(message)
	}
	return model.(Model)
}
