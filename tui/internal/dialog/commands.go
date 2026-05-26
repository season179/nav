package dialog

import (
	"fmt"
	"strings"

	"charm.land/bubbles/v2/key"
	"charm.land/bubbles/v2/textinput"
	tea "charm.land/bubbletea/v2"
	"github.com/sahilm/fuzzy"
	"nav.local/tui/internal/list"
)

// CommandAction identifies what to do when a command is chosen.
type CommandAction int

const (
	CommandNone CommandAction = iota
	CommandQuit
	CommandOpenModels
	CommandClearTranscript
)

// SelectedCommand is the result of choosing an item in the commands dialog.
type SelectedCommand struct {
	Action CommandAction
}

type commandItem struct {
	id       string
	title    string
	shortcut string
	aliases  []string
	action   CommandAction
}

func (i commandItem) Label() string    { return i.title }
func (i commandItem) Selectable() bool { return true }

func (i commandItem) filterText() string {
	parts := []string{i.title, i.id}
	parts = append(parts, i.aliases...)
	return strings.Join(parts, " ")
}

// Commands is a Crush-style command palette (opened with / or ctrl+p).
type Commands struct {
	list   *list.List
	filter textinput.Model
	items  []commandItem
	width  int
	height int
	active bool
}

const (
	commandsMaxWidth  = 72
	commandsMaxHeight = 20
)

var (
	commandsPrevious = key.NewBinding(
		key.WithKeys("up", "ctrl+p"),
		key.WithHelp("↑", "previous item"),
	)
	commandsNext = key.NewBinding(
		key.WithKeys("down", "ctrl+n"),
		key.WithHelp("↓", "next item"),
	)
	commandsEnter = key.NewBinding(
		key.WithKeys("enter"),
		key.WithHelp("enter", "confirm"),
	)
	commandsEsc = key.NewBinding(
		key.WithKeys("esc", "alt+esc"),
		key.WithHelp("esc", "cancel"),
	)
)

// NewCommands builds the command palette with nav's built-in system commands.
func NewCommands(width, height int) *Commands {
	c := &Commands{
		width:  min(width-4, commandsMaxWidth),
		height: min(height-6, commandsMaxHeight),
		active: true,
		items:  defaultCommandItems(),
	}

	ti := textinput.New()
	ti.Placeholder = "Type to filter"
	ti.CharLimit = 60
	ti.SetWidth(c.width - 6)
	ti.Focus()
	c.filter = ti

	c.list = list.New(toCommandListItems(c.items)...)
	c.list.SetSize(c.width-4, c.height-6)
	c.list.SelectFirst()
	c.list.ScrollToTop()

	return c
}

func defaultCommandItems() []commandItem {
	return []commandItem{
		{
			id:       "switch_model",
			title:    "Switch Model",
			shortcut: "ctrl+l",
			action:   CommandOpenModels,
		},
		{
			id:       "clear_transcript",
			title:    "Clear Transcript",
			shortcut: "",
			aliases:  []string{"clear", "reset"},
			action:   CommandClearTranscript,
		},
		{
			id:       "quit",
			title:    "Quit",
			shortcut: "ctrl+c",
			aliases:  []string{"exit", "quit", "/quit"},
			action:   CommandQuit,
		},
	}
}

// Active reports whether the dialog is shown.
func (c *Commands) Active() bool { return c != nil && c.active }

// HandleMsg processes input for the command palette.
func (c *Commands) HandleMsg(msg tea.Msg) (*SelectedCommand, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.KeyPressMsg:
		switch {
		case key.Matches(msg, commandsEsc):
			c.active = false
			return nil, nil
		case key.Matches(msg, commandsEnter):
			item := c.list.SelectedItem()
			if item == nil {
				return nil, nil
			}
			ci := item.(commandItem)
			c.active = false
			return &SelectedCommand{Action: ci.action}, nil
		case key.Matches(msg, commandsPrevious):
			if c.list.IsSelectedFirst() {
				c.list.SelectLast()
			} else {
				c.list.SelectPrev()
			}
			c.list.ScrollToSelected()
			return nil, nil
		case key.Matches(msg, commandsNext):
			if c.list.IsSelectedLast() {
				c.list.SelectFirst()
			} else {
				c.list.SelectNext()
			}
			c.list.ScrollToSelected()
			return nil, nil
		default:
			for _, item := range c.items {
				if msg.String() != "" && msg.String() == item.shortcut {
					c.active = false
					return &SelectedCommand{Action: item.action}, nil
				}
			}
		}
	case tea.KeyReleaseMsg:
		return nil, nil
	}

	var cmd tea.Cmd
	c.filter, cmd = c.filter.Update(msg)
	c.applyFilter()
	return nil, cmd
}

func (c *Commands) applyFilter() {
	query := normalizeFilterQuery(c.filter.Value())
	if query == "" {
		c.items = defaultCommandItems()
	} else {
		c.items = c.filterItemsFuzzy(query)
	}

	c.list.SetItems(toCommandListItems(c.items)...)
	c.list.SelectFirst()
	c.list.ScrollToTop()
}

func (c *Commands) filterItemsFuzzy(query string) []commandItem {
	all := defaultCommandItems()
	names := make([]string, len(all))
	for i, item := range all {
		names[i] = strings.ToLower(item.filterText())
	}

	matches := fuzzy.Find(query, names)
	filtered := make([]commandItem, 0, len(matches))
	for _, match := range matches {
		filtered = append(filtered, all[match.Index])
	}
	return filtered
}

// View renders the centered command palette.
func (c *Commands) View() string {
	if !c.Active() {
		return ""
	}

	w := c.width
	var b strings.Builder

	b.WriteString("┌")
	b.WriteString(strings.Repeat("─", w-2))
	b.WriteString("┐\n")

	title := padCenter("Commands", w-2)
	b.WriteString("│")
	b.WriteString(title)
	b.WriteString("│\n")

	b.WriteString("│ ")
	filterView := c.filter.View()
	b.WriteString(padRight(filterView, w-4))
	b.WriteString("  │\n")

	b.WriteString("├")
	b.WriteString(strings.Repeat("─", w-2))
	b.WriteString("┤\n")

	listHeight := c.height - 6
	c.list.SetSize(w-4, listHeight)
	start, end := c.list.VisibleRange()
	linesRendered := 0
	for i := start; i < end && linesRendered < listHeight; i++ {
		item := c.items[i]
		selected := i == c.list.Selected()
		line := c.renderItem(item, selected, w-4)
		b.WriteString("│ ")
		b.WriteString(padRight(line, w-4))
		b.WriteString(" │\n")
		linesRendered++
	}
	for linesRendered < listHeight {
		b.WriteString("│")
		b.WriteString(strings.Repeat(" ", w-2))
		b.WriteString("│\n")
		linesRendered++
	}

	scrollIndicator := ""
	if len(c.items) > listHeight {
		scrollIndicator = fmt.Sprintf(" %d/%d ", c.list.Selected()+1, len(c.items))
	}

	b.WriteString("├")
	b.WriteString(strings.Repeat("─", w-2))
	b.WriteString("┤\n")

	help := "↑/↓ choose  enter confirm  esc cancel"
	if scrollIndicator != "" {
		help = padRight(help, w-4-len(scrollIndicator)) + scrollIndicator
	} else {
		help = padRight(help, w-4)
	}
	b.WriteString("│ ")
	b.WriteString(help)
	b.WriteString(" │\n")

	b.WriteString("└")
	b.WriteString(strings.Repeat("─", w-2))
	b.WriteString("┘")

	return b.String()
}

func (c *Commands) renderItem(item commandItem, selected bool, width int) string {
	prefix := "  "
	if selected {
		prefix = "▸ "
	}

	title := prefix + item.title
	shortcut := item.shortcut
	if shortcut == "" {
		if selected {
			return "\033[7m" + padRight(title, width) + "\033[0m"
		}
		return truncate(title, width)
	}

	shortcutWidth := len(shortcut) + 1
	titleWidth := max(1, width-shortcutWidth)
	titlePart := truncate(title, titleWidth)
	gap := max(0, width-len(stripANSI(titlePart))-len(shortcut))

	line := titlePart + strings.Repeat(" ", gap) + shortcut
	if selected {
		return "\033[7m" + padRight(line, width) + "\033[0m"
	}
	return line
}

func toCommandListItems(items []commandItem) []list.Item {
	out := make([]list.Item, len(items))
	for i := range items {
		out[i] = items[i]
	}
	return out
}
