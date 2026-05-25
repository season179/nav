package main

import (
	"os"

	"nav.local/tui/internal/localdev"
)

var sourceRoot string

func main() {
	os.Exit(localdev.Run(os.Args[1:], sourceRoot))
}
