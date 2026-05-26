package ui

import "charm.land/bubbles/v2/key"

var (
	quitBinding = key.NewBinding(
		key.WithKeys("ctrl+c"),
		key.WithHelp("ctrl+c", "quit"),
	)
	sendBinding    = key.NewBinding(key.WithKeys("enter"))
	newlineBinding = key.NewBinding(key.WithKeys("ctrl+j"))
	slashCommandsBinding = key.NewBinding(
		key.WithKeys("/"),
		key.WithHelp("/", "commands"),
	)
	ctrlPCommandsBinding = key.NewBinding(
		key.WithKeys("ctrl+p"),
		key.WithHelp("ctrl+p", "commands"),
	)
	modelsBinding = key.NewBinding(
		key.WithKeys("ctrl+m", "ctrl+l"),
		key.WithHelp("ctrl+l", "models"),
	)
)
