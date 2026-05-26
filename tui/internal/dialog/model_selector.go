// Package dialog implements overlay dialogs for the nav TUI.
//
// Dialogs follow the same pattern as Crush's dialog system: they
// handle their own key events via HandleMsg and render themselves
// as centered boxes via View.
package dialog

import (
	"fmt"
	"sort"
	"strings"

	"charm.land/bubbles/v2/key"
	"charm.land/bubbles/v2/textinput"
	tea "charm.land/bubbletea/v2"
	"nav.local/tui/internal/list"
	"nav.local/tui/internal/settings"
)

// SelectedModel is the result of a model selection.
type SelectedModel struct {
	Provider string
	Model    string
}

// selectorItem is either a provider header or a model entry in the
// selector list.
type selectorItem struct {
	provider string // always set
	modelID  string // empty for provider headers
	name     string // display name
}

func (i selectorItem) Label() string    { return i.name }
func (i selectorItem) Selectable() bool { return i.modelID != "" }

// ModelSelector is a dialog for choosing a provider and model.
// It loads configured providers from ~/.nav/settings.json, groups
// models by provider, supports substring filtering, and writes the
// selection back to the settings file.
type ModelSelector struct {
	list     *list.List
	filter   textinput.Model
	items    []selectorItem // current (possibly filtered) list
	allItems []selectorItem // preserved for filter reset
	width    int
	height   int
	active   bool
}

const (
	selectorMaxWidth  = 72
	selectorMaxHeight = 20
)

// Key bindings for the model selector.
var (
	selectorUp    = key.NewBinding(key.WithKeys("up", "ctrl+p"))
	selectorDown  = key.NewBinding(key.WithKeys("down", "ctrl+n"))
	selectorEnter = key.NewBinding(key.WithKeys("enter"))
	selectorEsc   = key.NewBinding(key.WithKeys("esc"))
)

// NewModelSelector creates a model selector dialog populated from
// the given settings. Returns an error when no providers are configured.
func NewModelSelector(s settings.ModelSettings, width, height int) (*ModelSelector, error) {
	if len(s.Providers) == 0 {
		return nil, fmt.Errorf("no providers configured")
	}

	ms := &ModelSelector{
		width:  min(width-4, selectorMaxWidth),
		height: min(height-6, selectorMaxHeight),
		active: true,
	}

	ti := textinput.New()
	ti.Placeholder = "Filter models..."
	ti.CharLimit = 60
	ti.SetWidth(ms.width - 6)
	ti.Focus()
	ms.filter = ti

	ms.buildItems(s)
	ms.allItems = append([]selectorItem(nil), ms.items...)

	ms.list = list.New(toListItems(ms.items)...)
	ms.list.SetSize(ms.width-4, ms.height-6)

	// Select the current default model if present.
	if s.DefaultModel != nil {
		for i, item := range ms.items {
			if item.provider == s.DefaultModel.Provider && item.modelID == s.DefaultModel.Model {
				ms.list.SetSelected(i)
				break
			}
		}
	}

	ms.list.ScrollToSelected()
	return ms, nil
}

// buildItems creates the flat list of provider headers and model entries,
// sorted alphabetically by provider name.
func (ms *ModelSelector) buildItems(s settings.ModelSettings) {
	providers := make([]string, 0, len(s.Providers))
	for name := range s.Providers {
		providers = append(providers, name)
	}
	sort.Strings(providers)

	var items []selectorItem
	for _, pname := range providers {
		p := s.Providers[pname]
		items = append(items, selectorItem{
			provider: pname,
			name:     providerDisplayName(pname, p),
		})
		for _, m := range p.Models {
			items = append(items, selectorItem{
				provider: pname,
				modelID:  m.ID,
				name:     m.DisplayName(),
			})
		}
	}
	ms.items = items
}

func providerDisplayName(id string, p settings.ProviderEntry) string {
	if p.Name != "" {
		return p.Name
	}
	return id
}

// Active reports whether the dialog is currently shown.
func (ms *ModelSelector) Active() bool { return ms != nil && ms.active }

// HandleMsg processes a tea.Msg and returns a selected model
// (when the user confirms) and any pending tea.Cmd (for the
// filter textinput's cursor blink).
func (ms *ModelSelector) HandleMsg(msg tea.Msg) (*SelectedModel, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.KeyPressMsg:
		switch {
		case key.Matches(msg, selectorEsc):
			ms.active = false
			return nil, nil
		case key.Matches(msg, selectorEnter):
			item := ms.list.SelectedItem()
			if item == nil {
				return nil, nil
			}
			si := item.(selectorItem)
			if !si.Selectable() {
				return nil, nil
			}
			ms.active = false
			return &SelectedModel{
				Provider: si.provider,
				Model:    si.modelID,
			}, nil
		case key.Matches(msg, selectorUp):
			ms.list.SelectPrev()
			ms.list.ScrollToSelected()
			return nil, nil
		case key.Matches(msg, selectorDown):
			ms.list.SelectNext()
			ms.list.ScrollToSelected()
			return nil, nil
		}
	case tea.KeyReleaseMsg:
		return nil, nil
	}

	// Forward to filter input for text entry.
	var cmd tea.Cmd
	ms.filter, cmd = ms.filter.Update(msg)
	ms.applyFilter()
	return nil, cmd
}

// applyFilter rebuilds the visible item list from the current
// filter text using substring matching (case-insensitive).
func (ms *ModelSelector) applyFilter() {
	query := strings.ToLower(strings.TrimSpace(ms.filter.Value()))

	if query == "" {
		ms.items = append([]selectorItem(nil), ms.allItems...)
	} else {
		var filtered []selectorItem
		for _, item := range ms.allItems {
			if item.modelID == "" {
				continue // skip headers during filtering
			}
			label := strings.ToLower(item.name)
			provider := strings.ToLower(item.provider)
			if strings.Contains(label, query) || strings.Contains(provider, query) {
				filtered = append(filtered, item)
			}
		}
		ms.items = filtered
	}

	ms.list.SetItems(toListItems(ms.items)...)
	ms.list.ScrollToSelected()
}

// View renders the dialog as a centered box with title, filter
// input, scrollable model list, and help footer.
func (ms *ModelSelector) View() string {
	if !ms.Active() {
		return ""
	}

	w := ms.width
	var b strings.Builder

	// Top border
	b.WriteString("┌")
	b.WriteString(strings.Repeat("─", w-2))
	b.WriteString("┐\n")

	// Title
	title := padCenter("Switch Model", w-2)
	b.WriteString("│")
	b.WriteString(title)
	b.WriteString("│\n")

	// Filter input
	b.WriteString("│ ")
	filterView := ms.filter.View()
	b.WriteString(padRight(filterView, w-4))
	b.WriteString("  │\n")

	// Separator
	b.WriteString("├")
	b.WriteString(strings.Repeat("─", w-2))
	b.WriteString("┤\n")

	// Model list
	listHeight := ms.height - 6 // title + filter + separators + help + bottom
	ms.list.SetSize(w-4, listHeight)
	start, end := ms.list.VisibleRange()
	linesRendered := 0
	for i := start; i < end && linesRendered < listHeight; i++ {
		item := ms.items[i]
		selected := i == ms.list.Selected()
		line := ms.renderItem(item, selected, w-4)
		b.WriteString("│ ")
		b.WriteString(padRight(line, w-4))
		b.WriteString(" │\n")
		linesRendered++
	}
	// Pad remaining lines
	for linesRendered < listHeight {
		b.WriteString("│")
		b.WriteString(strings.Repeat(" ", w-2))
		b.WriteString("│\n")
		linesRendered++
	}

	// Scroll indicator
	scrollIndicator := ""
	if len(ms.items) > listHeight {
		scrollIndicator = fmt.Sprintf(" %d/%d ", ms.list.Selected()+1, len(ms.items))
	}

	// Bottom separator + help
	b.WriteString("├")
	b.WriteString(strings.Repeat("─", w-2))
	b.WriteString("┤\n")

	help := "↑↓ navigate  enter select  esc cancel"
	if scrollIndicator != "" {
		help = padRight(help, w-4-len(scrollIndicator)) + scrollIndicator
	} else {
		help = padRight(help, w-4)
	}
	b.WriteString("│ ")
	b.WriteString(help)
	b.WriteString(" │\n")

	// Bottom border
	b.WriteString("└")
	b.WriteString(strings.Repeat("─", w-2))
	b.WriteString("┘")

	return b.String()
}

// renderItem formats a single list item (header or model).
func (ms *ModelSelector) renderItem(item selectorItem, selected bool, width int) string {
	if item.modelID == "" {
		// Provider header — bold, not selectable
		return "\033[1m" + truncate(item.name, width) + "\033[0m"
	}
	prefix := "  "
	if selected {
		prefix = "▸ "
	}
	label := prefix + item.name
	if selected {
		// Inverse video for selected item
		return "\033[7m" + padRight(label, width) + "\033[0m"
	}
	return truncate(label, width)
}

// toListItems wraps selectorItems as list.Item values.
func toListItems(items []selectorItem) []list.Item {
	out := make([]list.Item, len(items))
	for i := range items {
		out[i] = items[i]
	}
	return out
}

// padCenter centers text within the given width.
func padCenter(s string, width int) string {
	if len(s) >= width {
		return s
	}
	left := (width - len(s)) / 2
	right := width - len(s) - left
	return strings.Repeat(" ", left) + s + strings.Repeat(" ", right)
}

// padRight right-pads text to the given width.
func padRight(s string, width int) string {
	visible := stripANSI(s)
	if len(visible) >= width {
		return s
	}
	return s + strings.Repeat(" ", width-len(visible))
}

// truncate shortens text to fit within width, adding "…" if needed.
func truncate(s string, width int) string {
	if len(s) <= width {
		return s
	}
	if width <= 1 {
		return s[:width]
	}
	return s[:width-1] + "…"
}

// stripANSI removes ANSI escape sequences for width calculation.
func stripANSI(s string) string {
	var b strings.Builder
	inEscape := false
	for _, r := range s {
		if r == '\033' {
			inEscape = true
			continue
		}
		if inEscape {
			if r == 'm' {
				inEscape = false
			}
			continue
		}
		b.WriteRune(r)
	}
	return b.String()
}
