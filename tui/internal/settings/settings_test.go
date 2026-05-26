package settings

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

func TestLoadReturnsZeroSettingsWhenFileDoesNotExist(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "missing.json")

	s, err := LoadFrom(path)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if s.DefaultModel != nil {
		t.Fatalf("expected nil default model, got %v", s.DefaultModel)
	}
	if len(s.Providers) != 0 {
		t.Fatalf("expected no providers, got %d", len(s.Providers))
	}
}

func TestLoadParsesProvidersAndDefaultModel(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "settings.json")

	data := `{
		"defaultModel": {"provider": "openai", "model": "gpt-4"},
		"providers": {
			"openai": {
				"baseUrl": "https://api.openai.com/v1",
				"models": [
					{"id": "gpt-4", "name": "GPT-4"},
					{"id": "gpt-4o", "name": "GPT-4o"}
				]
			},
			"anthropic": {
				"name": "Anthropic",
				"baseUrl": "https://api.anthropic.com",
				"models": [
					{"id": "claude-3", "name": "Claude 3"}
				]
			}
		}
	}`
	if err := os.WriteFile(path, []byte(data), 0o644); err != nil {
		t.Fatal(err)
	}

	s, err := LoadFrom(path)
	if err != nil {
		t.Fatalf("load error: %v", err)
	}

	if s.DefaultModel == nil {
		t.Fatal("expected default model to be set")
	}
	if s.DefaultModel.Provider != "openai" {
		t.Fatalf("provider = %q, want openai", s.DefaultModel.Provider)
	}
	if s.DefaultModel.Model != "gpt-4" {
		t.Fatalf("model = %q, want gpt-4", s.DefaultModel.Model)
	}
	if len(s.Providers) != 2 {
		t.Fatalf("providers = %d, want 2", len(s.Providers))
	}

	oai := s.Providers["openai"]
	if len(oai.Models) != 2 {
		t.Fatalf("openai models = %d, want 2", len(oai.Models))
	}
	if oai.Models[0].DisplayName() != "GPT-4" {
		t.Fatalf("first model name = %q, want GPT-4", oai.Models[0].DisplayName())
	}

	ant := s.Providers["anthropic"]
	if ant.Name != "Anthropic" {
		t.Fatalf("anthropic name = %q, want Anthropic", ant.Name)
	}
}

func TestModelEntryDisplayNameFallsBackToID(t *testing.T) {
	m := ModelEntry{ID: "gpt-4-turbo"}
	if m.DisplayName() != "gpt-4-turbo" {
		t.Fatalf("DisplayName() = %q, want gpt-4-turbo", m.DisplayName())
	}

	m2 := ModelEntry{ID: "gpt-4", Name: "GPT-4"}
	if m2.DisplayName() != "GPT-4" {
		t.Fatalf("DisplayName() = %q, want GPT-4", m2.DisplayName())
	}
}

func TestWriteDefaultModelPreservesUnknownFields(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "settings.json")

	// Write initial settings with extra fields.
	initial := `{
		"defaultModel": {"provider": "old", "model": "old-model"},
		"providers": {
			"openai": {
				"baseUrl": "https://api.openai.com/v1",
				"apiKey": {"envVar": "OPENAI_API_KEY"},
				"compat": {"thinkingFormat": "openai"},
				"models": [
					{
						"id": "gpt-4",
						"name": "GPT-4",
						"thinkingLevelMap": {"off": null, "high": "high"},
						"cost": {"input": 30, "output": 60}
					}
				]
			}
		},
		"recentModels": [{"provider": "openai", "model": "gpt-4"}]
	}`
	if err := os.WriteFile(path, []byte(initial), 0o644); err != nil {
		t.Fatal(err)
	}

	// Update the default model.
	if err := WriteDefaultModelTo(path, "openai", "gpt-4"); err != nil {
		t.Fatalf("write error: %v", err)
	}

	// Read back and verify.
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}

	var raw map[string]json.RawMessage
	if err := json.Unmarshal(data, &raw); err != nil {
		t.Fatalf("parse error: %v", err)
	}

	// Check defaultModel was updated.
	var dm ModelRef
	if err := json.Unmarshal(raw["defaultModel"], &dm); err != nil {
		t.Fatal(err)
	}
	if dm.Provider != "openai" || dm.Model != "gpt-4" {
		t.Fatalf("defaultModel = %v, want openai/gpt-4", dm)
	}

	// Check providers preserved.
	if _, ok := raw["providers"]; !ok {
		t.Fatal("providers field was lost")
	}

	// Check recentModels preserved.
	if _, ok := raw["recentModels"]; !ok {
		t.Fatal("recentModels field was lost")
	}

	// Verify nested unknown fields survived by re-parsing.
	var s ModelSettings
	if err := json.Unmarshal(data, &s); err != nil {
		t.Fatalf("re-parse error: %v", err)
	}
	oai := s.Providers["openai"]
	if len(oai.Models) != 1 || oai.Models[0].ID != "gpt-4" {
		t.Fatalf("provider models lost: %v", oai.Models)
	}
}

func TestWriteDefaultModelCreatesFileAndDirectory(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "sub", "settings.json")

	if err := WriteDefaultModelTo(path, "anthropic", "claude-3"); err != nil {
		t.Fatalf("write error: %v", err)
	}

	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}

	var raw map[string]json.RawMessage
	if err := json.Unmarshal(data, &raw); err != nil {
		t.Fatal(err)
	}

	var dm ModelRef
	if err := json.Unmarshal(raw["defaultModel"], &dm); err != nil {
		t.Fatal(err)
	}
	if dm.Provider != "anthropic" || dm.Model != "claude-3" {
		t.Fatalf("defaultModel = %v, want anthropic/claude-3", dm)
	}
}

func TestWriteDefaultModelOverwritesCorruptFile(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "settings.json")

	if err := os.WriteFile(path, []byte("not json{{{"), 0o644); err != nil {
		t.Fatal(err)
	}

	if err := WriteDefaultModelTo(path, "test", "model"); err != nil {
		t.Fatalf("write error: %v", err)
	}

	s, err := LoadFrom(path)
	if err != nil {
		t.Fatalf("load after write error: %v", err)
	}
	if s.DefaultModel == nil || s.DefaultModel.Provider != "test" {
		t.Fatalf("defaultModel = %v, want test/model", s.DefaultModel)
	}
}
