package ui

import "charm.land/bubbles/v2/key"

var (
	quitBinding    = key.NewBinding(key.WithKeys("ctrl+c"))
	sendBinding    = key.NewBinding(key.WithKeys("enter"))
	newlineBinding = key.NewBinding(key.WithKeys("ctrl+j"))
	modelsBinding  = key.NewBinding(key.WithKeys("ctrl+m", "ctrl+l"))
)
