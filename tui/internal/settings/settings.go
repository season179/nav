// Package settings reads and writes nav's model configuration file.
//
// The file format matches the Rust backend's ModelSettings schema:
//
//	{
//	  "defaultModel": {"provider": "openai", "model": "gpt-4"},
//	  "providers": { ... }
//	}
//
// The reader preserves unknown JSON fields when writing back so that
// backend-specific fields (compat, thinkingLevelMap, etc.) survive
// round-trips through the TUI.
package settings

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
)

const defaultSettingsPath = "~/.nav/settings.json"

// ModelSettings mirrors the Rust ModelSettings struct.
type ModelSettings struct {
	DefaultModel *ModelRef               `json:"defaultModel,omitempty"`
	Providers    map[string]ProviderEntry `json:"providers,omitempty"`
}

// ModelRef identifies a specific model within a provider.
type ModelRef struct {
	Provider string `json:"provider"`
	Model    string `json:"model"`
}

// ProviderEntry holds the user-visible fields of a provider config.
// Unknown fields are preserved through the raw JSON round-trip in
// WriteDefaultModel.
type ProviderEntry struct {
	Name    string       `json:"name,omitempty"`
	BaseURL string       `json:"baseUrl,omitempty"`
	API     string       `json:"api,omitempty"`
	Models  []ModelEntry `json:"models,omitempty"`
}

// ModelEntry holds the user-visible fields of a model config.
type ModelEntry struct {
	ID   string `json:"id"`
	Name string `json:"name,omitempty"`
}

// DisplayName returns the model's display name, falling back to the ID.
func (m ModelEntry) DisplayName() string {
	if m.Name != "" {
		return m.Name
	}
	return m.ID
}

// Load reads the model settings from the default path.
// Returns a zero-value ModelSettings if the file does not exist.
func Load() (ModelSettings, error) {
	return LoadFrom(DefaultPath())
}

// LoadFrom reads the model settings from the given path.
func LoadFrom(path string) (ModelSettings, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		if os.IsNotExist(err) {
			return ModelSettings{}, nil
		}
		return ModelSettings{}, err
	}
	var s ModelSettings
	if err := json.Unmarshal(data, &s); err != nil {
		return ModelSettings{}, err
	}
	return s, nil
}

// WriteDefaultModel updates only the defaultModel field in the settings
// file, preserving all other fields verbatim.
func WriteDefaultModel(provider, model string) error {
	return WriteDefaultModelTo(DefaultPath(), provider, model)
}

// WriteDefaultModelTo is like WriteDefaultModel but with an explicit path.
func WriteDefaultModelTo(path, provider, model string) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}

	data, err := os.ReadFile(path)
	if err != nil && !os.IsNotExist(err) {
		return err
	}

	// Parse as generic JSON to preserve unknown fields.
	var raw map[string]json.RawMessage
	if len(data) > 0 {
		if err := json.Unmarshal(data, &raw); err != nil {
			raw = make(map[string]json.RawMessage)
		}
	} else {
		raw = make(map[string]json.RawMessage)
	}

	dm := map[string]string{
		"provider": provider,
		"model":    model,
	}
	dmBytes, err := json.Marshal(dm)
	if err != nil {
		return err
	}
	raw["defaultModel"] = dmBytes

	out, err := json.MarshalIndent(raw, "", "  ")
	if err != nil {
		return err
	}
	return os.WriteFile(path, append(out, '\n'), 0o644)
}

// DefaultPath returns the resolved default settings file path.
func DefaultPath() string {
	if p := os.Getenv("NAV_MODEL_SETTINGS"); p != "" {
		return expandHome(p)
	}
	return expandHome(defaultSettingsPath)
}

func expandHome(path string) string {
	if path == "~" || strings.HasPrefix(path, "~/") {
		if home, err := os.UserHomeDir(); err == nil {
			return filepath.Join(home, strings.TrimPrefix(path, "~"))
		}
	}
	return path
}
