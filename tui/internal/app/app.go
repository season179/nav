package app

import (
	"context"
	"fmt"
	"os"

	tea "charm.land/bubbletea/v2"
	"nav.local/tui/internal/backend"
	"nav.local/tui/internal/ui"
)

func Run(backendPath string) int {
	client := backend.NewClientWithBackendPath(backendPath)
	program := tea.NewProgram(ui.New(client), tea.WithContext(context.Background()))
	if _, err := program.Run(); err != nil {
		fmt.Fprintf(os.Stderr, "nav crashed: %v\n", err)
		return 1
	}
	return 0
}
