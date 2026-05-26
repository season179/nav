package dialog

import (
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"nav.local/tui/internal/settings"
)

func TestNewModelSelectorWithEmptyProviders(t *testing.T) {
	s := settings.ModelSettings{}
	_, err := NewModelSelector(s, 80, 24)
	if err == nil {
		t.Fatal("expected error for empty providers")
	}
}

func TestNewModelSelectorBuildsItems(t *testing.T) {
	s := settings.ModelSettings{
		Providers: map[string]settings.ProviderEntry{
			"openai": {
				Name: "OpenAI",
				Models: []settings.ModelEntry{
					{ID: "gpt-4", Name: "GPT-4"},
					{ID: "gpt-4o", Name: "GPT-4o"},
				},
			},
			"anthropic": {
				Models: []settings.ModelEntry{
					{ID: "claude-3", Name: "Claude 3"},
				},
			},
		},
	}

	ms, err := NewModelSelector(s, 80, 24)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if !ms.Active() {
		t.Fatal("expected selector to be active")
	}

	// Should have 2 headers + 3 models = 5 items.
	if len(ms.allItems) != 5 {
		t.Fatalf("allItems = %d, want 5", len(ms.allItems))
	}

	// First item should be a header.
	if ms.allItems[0].Selectable() {
		t.Fatal("first item should be a header (not selectable)")
	}
}

func TestModelSelectorSelectsCurrentDefault(t *testing.T) {
	s := settings.ModelSettings{
		DefaultModel: &settings.ModelRef{Provider: "openai", Model: "gpt-4o"},
		Providers: map[string]settings.ProviderEntry{
			"openai": {
				Models: []settings.ModelEntry{
					{ID: "gpt-4", Name: "GPT-4"},
					{ID: "gpt-4o", Name: "GPT-4o"},
				},
			},
		},
	}

	ms, err := NewModelSelector(s, 80, 24)
	if err != nil {
		t.Fatal(err)
	}

	// Should select gpt-4o (index 2: header, gpt-4, gpt-4o).
	if ms.list.Selected() != 2 {
		t.Fatalf("Selected() = %d, want 2", ms.list.Selected())
	}
}

func TestModelSelectorFilter(t *testing.T) {
	s := settings.ModelSettings{
		Providers: map[string]settings.ProviderEntry{
			"openai": {
				Models: []settings.ModelEntry{
					{ID: "gpt-4", Name: "GPT-4"},
					{ID: "gpt-4o", Name: "GPT-4o"},
					{ID: "gpt-3.5", Name: "GPT-3.5"},
				},
			},
		},
	}

	ms, err := NewModelSelector(s, 80, 24)
	if err != nil {
		t.Fatal(err)
	}

	// Type "4o" to filter.
	ms.filter.SetValue("4o")
	ms.applyFilter()

	// Should show the provider header and the matching model.
	if len(ms.items) != 2 {
		t.Fatalf("filtered items = %d, want 2", len(ms.items))
	}
	if ms.items[0].modelID != "" {
		t.Fatal("first filtered item should be provider header")
	}
	if ms.items[1].modelID != "gpt-4o" {
		t.Fatalf("filtered item = %q, want gpt-4o", ms.items[1].modelID)
	}
}

func TestModelSelectorFilterByProvider(t *testing.T) {
	s := settings.ModelSettings{
		Providers: map[string]settings.ProviderEntry{
			"openai": {
				Models: []settings.ModelEntry{
					{ID: "gpt-4", Name: "GPT-4"},
				},
			},
			"anthropic": {
				Models: []settings.ModelEntry{
					{ID: "claude-3", Name: "Claude 3"},
				},
			},
		},
	}

	ms, err := NewModelSelector(s, 80, 24)
	if err != nil {
		t.Fatal(err)
	}

	ms.filter.SetValue("anthropic")
	ms.applyFilter()

	if len(ms.items) != 2 {
		t.Fatalf("filtered items = %d, want 2", len(ms.items))
	}
	if ms.items[1].provider != "anthropic" {
		t.Fatalf("filtered provider = %q, want anthropic", ms.items[1].provider)
	}
}

func TestModelSelectorHandleMsgEsc(t *testing.T) {
	s := settings.ModelSettings{
		Providers: map[string]settings.ProviderEntry{
			"openai": {Models: []settings.ModelEntry{{ID: "gpt-4"}}},
		},
	}

	ms, err := NewModelSelector(s, 80, 24)
	if err != nil {
		t.Fatal(err)
	}

	sel, _ := ms.HandleMsg(tea.KeyPressMsg{Code: tea.KeyEsc})
	if sel != nil {
		t.Fatal("esc should not select a model")
	}
	if ms.Active() {
		t.Fatal("esc should close the selector")
	}
}

func TestModelSelectorHandleMsgEnter(t *testing.T) {
	s := settings.ModelSettings{
		Providers: map[string]settings.ProviderEntry{
			"openai": {
				Models: []settings.ModelEntry{
					{ID: "gpt-4", Name: "GPT-4"},
				},
			},
		},
	}

	ms, err := NewModelSelector(s, 80, 24)
	if err != nil {
		t.Fatal(err)
	}

	// Selected starts at first model (index 1 after header).
	sel, _ := ms.HandleMsg(tea.KeyPressMsg{Code: tea.KeyEnter})
	if sel == nil {
		t.Fatal("enter should select a model")
	}
	if sel.Provider != "openai" || sel.Model != "gpt-4" {
		t.Fatalf("selected = %v, want openai/gpt-4", sel)
	}
	if ms.Active() {
		t.Fatal("enter should close the selector")
	}
}

func TestModelSelectorHandleMsgUpDown(t *testing.T) {
	s := settings.ModelSettings{
		Providers: map[string]settings.ProviderEntry{
			"openai": {
				Models: []settings.ModelEntry{
					{ID: "gpt-4", Name: "GPT-4"},
					{ID: "gpt-4o", Name: "GPT-4o"},
				},
			},
		},
	}

	ms, err := NewModelSelector(s, 80, 24)
	if err != nil {
		t.Fatal(err)
	}

	start := ms.list.Selected()
	ms.HandleMsg(tea.KeyPressMsg{Code: tea.KeyDown})
	if ms.list.Selected() <= start {
		t.Fatal("down should move selection forward")
	}

	ms.HandleMsg(tea.KeyPressMsg{Code: tea.KeyUp})
	if ms.list.Selected() != start {
		t.Fatal("up should move selection back")
	}
}

func TestModelSelectorView(t *testing.T) {
	s := settings.ModelSettings{
		Providers: map[string]settings.ProviderEntry{
			"openai": {
				Name: "OpenAI",
				Models: []settings.ModelEntry{
					{ID: "gpt-4", Name: "GPT-4"},
				},
			},
		},
	}

	ms, err := NewModelSelector(s, 80, 24)
	if err != nil {
		t.Fatal(err)
	}

	view := ms.View()
	if view == "" {
		t.Fatal("View() should not be empty")
	}

	// Check for key elements.
	if !strings.Contains(view, "Switch Model") {
		t.Error("view should contain title")
	}
	if !strings.Contains(view, "OpenAI") {
		t.Error("view should contain provider name")
	}
	if !strings.Contains(view, "GPT-4") {
		t.Error("view should contain model name")
	}
	if !strings.Contains(view, "enter confirm") {
		t.Error("view should contain help text")
	}
}

func TestModelSelectorFilterFuzzyMatch(t *testing.T) {
	s := settings.ModelSettings{
		Providers: map[string]settings.ProviderEntry{
			"openai": {
				Models: []settings.ModelEntry{
					{ID: "gpt-4o", Name: "GPT-4o"},
				},
			},
		},
	}

	ms, err := NewModelSelector(s, 80, 24)
	if err != nil {
		t.Fatal(err)
	}

	// "gpto" should still match "gpt-4o" via fuzzy search.
	ms.filter.SetValue("gpto")
	ms.applyFilter()

	if len(ms.items) != 2 {
		t.Fatalf("filtered items = %d, want 2", len(ms.items))
	}
	if ms.items[1].modelID != "gpt-4o" {
		t.Fatalf("filtered item = %q, want gpt-4o", ms.items[1].modelID)
	}
}

func TestModelSelectorPreviousWrapsFromFirst(t *testing.T) {
	s := settings.ModelSettings{
		Providers: map[string]settings.ProviderEntry{
			"openai": {
				Models: []settings.ModelEntry{
					{ID: "gpt-4", Name: "GPT-4"},
					{ID: "gpt-4o", Name: "GPT-4o"},
				},
			},
		},
	}

	ms, err := NewModelSelector(s, 80, 24)
	if err != nil {
		t.Fatal(err)
	}

	if !ms.list.IsSelectedFirst() {
		t.Fatal("expected initial selection on first model")
	}

	ms.HandleMsg(tea.KeyPressMsg{Code: tea.KeyUp})
	if ms.list.Selected() != 2 {
		t.Fatalf("Selected() = %d, want 2 after wrap from first", ms.list.Selected())
	}
}

func TestModelSelectorViewWhenInactive(t *testing.T) {
	s := settings.ModelSettings{
		Providers: map[string]settings.ProviderEntry{
			"openai": {Models: []settings.ModelEntry{{ID: "gpt-4"}}},
		},
	}

	ms, err := NewModelSelector(s, 80, 24)
	if err != nil {
		t.Fatal(err)
	}

	ms.active = false
	view := ms.View()
	if view != "" {
		t.Fatalf("inactive selector should return empty view, got %q", view)
	}
}
