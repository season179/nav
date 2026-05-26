package dialog

import (
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
)

func TestNewCommandsActive(t *testing.T) {
	c := NewCommands(80, 24)
	if !c.Active() {
		t.Fatal("expected commands dialog to be active")
	}
	if len(c.items) < 3 {
		t.Fatalf("items = %d, want at least 3 commands", len(c.items))
	}
}

func TestCommandsFilterQuitAlias(t *testing.T) {
	c := NewCommands(80, 24)
	c.filter.SetValue("/quit")
	c.applyFilter()

	if len(c.items) != 1 {
		t.Fatalf("filtered items = %d, want 1", len(c.items))
	}
	if c.items[0].action != CommandQuit {
		t.Fatalf("action = %v, want quit", c.items[0].action)
	}
}

func TestCommandsSelectQuit(t *testing.T) {
	c := NewCommands(80, 24)
	c.filter.SetValue("quit")
	c.applyFilter()

	sel, _ := c.HandleMsg(tea.KeyPressMsg{Code: tea.KeyEnter})
	if sel == nil || sel.Action != CommandQuit {
		t.Fatalf("selected = %v, want quit", sel)
	}
	if c.Active() {
		t.Fatal("dialog should close after selection")
	}
}

func TestCommandsEscCloses(t *testing.T) {
	c := NewCommands(80, 24)
	sel, _ := c.HandleMsg(tea.KeyPressMsg{Code: tea.KeyEsc})
	if sel != nil {
		t.Fatal("esc should not select a command")
	}
	if c.Active() {
		t.Fatal("esc should close the dialog")
	}
}

func TestCommandsView(t *testing.T) {
	c := NewCommands(80, 24)
	view := c.View()
	for _, want := range []string{"Commands", "Quit", "Switch Model", "enter confirm"} {
		if !strings.Contains(view, want) {
			t.Errorf("view missing %q", want)
		}
	}
}
