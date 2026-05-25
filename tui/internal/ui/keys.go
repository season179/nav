package ui

import "charm.land/bubbles/v2/key"

var (
	quitBinding    = key.NewBinding(key.WithKeys("ctrl+c", "esc"))
	sendBinding    = key.NewBinding(key.WithKeys("enter"))
	newlineBinding = key.NewBinding(key.WithKeys("ctrl+j"))
)
