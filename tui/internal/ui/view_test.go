package ui

import (
	"strings"
	"testing"
)

func TestRenderThinkingDoesNotPadShortThinkingToTailWindow(t *testing.T) {
	rendered := renderThinking("thinking", 40)

	if got := strings.Count(rendered, "\n") + 1; got != 1 {
		t.Fatalf("rendered thinking line count = %d, want 1; rendered=%q", got, rendered)
	}
}

func TestLastLinesKeepsNewestLinesWithoutPadding(t *testing.T) {
	rendered := lastLines(strings.Join([]string{
		"one",
		"two",
		"three",
		"four",
		"five",
		"six",
		"seven",
		"eight",
		"nine",
	}, "\n"), maxThinkingDisplayLines)

	if strings.Contains(rendered, "one") {
		t.Fatalf("rendered thinking retained oldest line: %q", rendered)
	}
	if !strings.Contains(rendered, "nine") {
		t.Fatalf("rendered thinking dropped newest line: %q", rendered)
	}
}

func TestLastLinesHandlesZeroLimit(t *testing.T) {
	if got := lastLines("thinking", 0); got != "" {
		t.Fatalf("lastLines with zero limit = %q, want empty string", got)
	}
}
