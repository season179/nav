package ui

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
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

	// ctrl+c quits (esc now closes dialogs, matching Crush's keybindings).
	updated, _ = updated.Update(tea.KeyPressMsg{Code: 'c', Mod: tea.ModCtrl})

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

func TestSlashOpensCommandsWhenComposerEmpty(t *testing.T) {
	model := New(&fakeAgent{})
	model.width = 80
	model.height = 24

	updated, cmd := model.Update(tea.KeyPressMsg{Code: '/'})
	result := updated.(Model)
	if cmd != nil {
		t.Fatal("openCommands should not return a command")
	}
	if result.commands == nil || !result.commands.Active() {
		t.Fatal("expected commands dialog after /")
	}
}

func TestCtrlPOpensCommands(t *testing.T) {
	model := New(&fakeAgent{})
	model.width = 80
	model.height = 24
	model.composer.SetValue("draft text")

	updated, _ := model.Update(tea.KeyPressMsg{Code: 'p', Mod: tea.ModCtrl})
	result := updated.(Model)
	if result.commands == nil || !result.commands.Active() {
		t.Fatal("ctrl+p should open commands even when composer has text")
	}
}

func TestCommandsQuitExits(t *testing.T) {
	agent := &fakeAgent{}
	model := New(agent)
	model.width = 80
	model.height = 24

	updated, _ := model.Update(tea.KeyPressMsg{Code: '/'})
	active := updated.(Model)

	// Navigate to Quit (third item).
	updated, _ = active.Update(tea.KeyPressMsg{Code: tea.KeyDown})
	updated, _ = updated.Update(tea.KeyPressMsg{Code: tea.KeyDown})
	updated, cmd := updated.Update(tea.KeyPressMsg{Code: tea.KeyEnter})
	if cmd == nil {
		t.Fatal("quit command should schedule exit")
	}

	// First batch step closes the agent; second is tea.Quit.
	msg := cmd()
	if msg == nil {
		t.Fatal("expected quit sequence message")
	}
	if followUp, ok := msg.(tea.Cmd); ok && followUp != nil {
		if quit := followUp(); quit == nil {
			t.Fatal("expected tea.Quit from command palette")
		}
	}
}

func TestModelsBindingOpensSelector(t *testing.T) {
	path := writeModelSettingsFile(t, `{
		"defaultModel": {"provider": "openai", "model": "gpt-4"},
		"providers": {
			"openai": {
				"models": [
					{"id": "gpt-4", "name": "GPT-4"},
					{"id": "gpt-4o", "name": "GPT-4o"}
				]
			}
		}
	}`)
	t.Setenv("NAV_MODEL_SETTINGS", path)

	model := New(&fakeAgent{})
	model.width = 80
	model.height = 24

	updated, cmd := model.Update(tea.KeyPressMsg{Code: 'l', Mod: tea.ModCtrl})
	result := updated.(Model)
	if cmd != nil {
		t.Fatal("openModelSelector should not return a command")
	}
	if result.modelSelector == nil || !result.modelSelector.Active() {
		t.Fatal("ctrl+l should open the model selector")
	}
}

func TestModelsBindingCtrlMOpensSelector(t *testing.T) {
	path := writeModelSettingsFile(t, `{
		"providers": {
			"openai": {"models": [{"id": "gpt-4"}]}
		}
	}`)
	t.Setenv("NAV_MODEL_SETTINGS", path)

	model := New(&fakeAgent{})
	model.width = 80
	model.height = 24

	updated, _ := model.Update(tea.KeyPressMsg{Code: 'm', Mod: tea.ModCtrl})
	result := updated.(Model)
	if result.modelSelector == nil || !result.modelSelector.Active() {
		t.Fatal("ctrl+m should open the model selector")
	}
}

func TestSelectModelPersistsSettingsAndReloadsBackend(t *testing.T) {
	path := writeModelSettingsFile(t, `{
		"defaultModel": {"provider": "openai", "model": "gpt-4"},
		"providers": {
			"openai": {
				"models": [
					{"id": "gpt-4", "name": "GPT-4"},
					{"id": "gpt-4o", "name": "GPT-4o"}
				]
			}
		}
	}`)
	t.Setenv("NAV_MODEL_SETTINGS", path)

	agent := &fakeAgent{}
	model := New(agent)
	model.width = 80
	model.height = 24

	updated, _ := model.Update(tea.KeyPressMsg{Code: 'l', Mod: tea.ModCtrl})
	active := updated.(Model)
	if active.modelSelector == nil {
		t.Fatal("model selector not open")
	}

	updated, _ = active.Update(tea.KeyPressMsg{Code: tea.KeyDown})
	updated, cmd := updated.(Model).Update(tea.KeyPressMsg{Code: tea.KeyEnter})
	if cmd == nil {
		t.Fatal("enter should schedule model selection")
	}

	result := runUntilIdle(t, updated, cmd)
	if !agent.reloaded {
		t.Fatal("expected backend settings reload after model selection")
	}
	if result.currentModel != "openai/gpt-4o" {
		t.Fatalf("currentModel = %q, want openai/gpt-4o", result.currentModel)
	}

	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	var raw map[string]json.RawMessage
	if err := json.Unmarshal(data, &raw); err != nil {
		t.Fatal(err)
	}
	var dm map[string]string
	if err := json.Unmarshal(raw["defaultModel"], &dm); err != nil {
		t.Fatal(err)
	}
	if dm["provider"] != "openai" || dm["model"] != "gpt-4o" {
		t.Fatalf("defaultModel = %v, want openai/gpt-4o", dm)
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
	reloaded     bool
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

func (a *fakeAgent) ReloadSettings(context.Context) error {
	a.reloaded = true
	return nil
}

func writeModelSettingsFile(t *testing.T, contents string) string {
	t.Helper()
	path := filepath.Join(t.TempDir(), "settings.json")
	if err := os.WriteFile(path, []byte(contents), 0o644); err != nil {
		t.Fatal(err)
	}
	return path
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
