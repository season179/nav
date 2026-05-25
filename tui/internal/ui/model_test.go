package ui

import (
	"context"
	"testing"

	"nav.local/tui/internal/client"
)

func TestSubmitComposerRendersAssistantDeltasFromBackendEvents(t *testing.T) {
	agent := &fakeAgent{
		events: []client.Event{
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
	message := cmd()
	updated, _ := next.Update(message)
	result := updated.(Model)

	if agent.sentText != "hello backend" {
		t.Fatalf("sent text %q, want %q", agent.sentText, "hello backend")
	}
	last := result.messages[len(result.messages)-1]
	if last.Role != "assistant" || last.Body != "hello from backend" {
		t.Fatalf("last transcript item = %#v, want assistant backend response", last)
	}
	if result.status != "ready" {
		t.Fatalf("status = %q, want ready", result.status)
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
	message := cmd()
	updated, _ := next.Update(message)
	result := updated.(Model)

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

type fakeAgent struct {
	sentText string
	events   []client.Event
}

func (a *fakeAgent) Connect(context.Context) (client.SessionInfo, error) {
	return client.SessionInfo{SessionID: "session-1", Endpoint: "http://backend.test", CWD: "/tmp/nav"}, nil
}

func (a *fakeAgent) SendMessage(_ context.Context, text string) ([]client.Event, error) {
	a.sentText = text
	return a.events, nil
}

func (a *fakeAgent) Close() error {
	return nil
}
