#!/usr/bin/env bash
set -euo pipefail

# Stream-tail visibility smoke test
# Verifies that during a live stream that overflows the history viewport,
# the newest output stays visible near the bottom and the composer remains
# pinned to the terminal bottom.
#
# This test exercises the rendered terminal surface during a live stream,
# not only the reducer.

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
TUI="$ROOT/tui"
SESSION="nav-stream-tail-smoke"
TMP="$(mktemp -d)"
TMUX_WIDTH=120
TMUX_HEIGHT=30
FINAL_TEXT="Final assistant message: residue check complete."

# Absolute path for cleanup (pkill needs it); relative NAV_BACKEND is set later
# for the tmux session which cd's into $TUI.
E2E_BACKEND="$TUI/scripts/nav-e2e-backend.ts"

cleanup() {
	tmux kill-session -t "$SESSION" 2>/dev/null || true
	pkill -f "$E2E_BACKEND serve-http" 2>/dev/null || true
	rm -rf "$TMP"
}
trap cleanup EXIT

# --- helpers ---------------------------------------------------------------

fail() {
	echo "FAIL: $1"
	echo "--- tmux pane ---"
	cat "$OUT" 2>/dev/null || true
	exit 1
}

wait_for() {
	local needle="$1"
	local attempts="${2:-40}"
	for _ in $(seq 1 "$attempts"); do
		tmux capture-pane -p -t "$SESSION" -S -300 >"$OUT" || true
		if grep -Fq "$needle" "$OUT"; then
			return 0
		fi
		sleep 0.25
	done
	return 1
}

send_wheel_down() {
	tmux send-keys -t "$SESSION" -H 1b 5b 3c 36 35 3b 31 3b 31 30 4d
}

# --- install dependencies if needed ----------------------------------------

echo "==> Installing dependencies if needed"
cd "$TUI" && bun install 2>&1 | tail -1
cd - >/dev/null

echo "==> Launching Ink TUI in tmux (${SESSION}, ${TMUX_WIDTH}x${TMUX_HEIGHT})"
tmux kill-session -t "$SESSION" 2>/dev/null || true
tmux new-session -d -s "$SESSION" -x "$TMUX_WIDTH" -y "$TMUX_HEIGHT" \
	"cd \"$TUI\" && exec env NAV_BACKEND=\"./scripts/nav-e2e-backend.ts\" bun run start"
sleep 1

OUT="$TMP/pane.txt"

# --- wait for composer to be ready -----------------------------------------

echo "==> Waiting for composer to be ready"
wait_for "/model" 60 || fail "composer did not reach ready state"

echo "==> Verifying initial layout (composer at bottom)"
tmux capture-pane -p -t "$SESSION" >"$OUT"
if ! tail -n 6 "$OUT" | tr -d '\n' | grep -Fq "Enter send"; then
	fail "composer should be visible at the bottom of the terminal"
fi

# --- send a prompt that generates a long stream ----------------------------

echo "==> Sending prompt to trigger long stream"
# The e2e backend emits 12 file.changed + 6 tool calls + assistant text.
# With height=30 this should overflow the viewport.
tmux set-buffer -b nav-stream-smoke 'Run the deterministic tmux residue smoke.'
tmux paste-buffer -b nav-stream-smoke -t "$SESSION" -p
sleep 0.2
tmux send-keys -t "$SESSION" Enter

echo "==> Waiting for stream to start"
wait_for "args command:" 30 || fail "stream did not start"

# --- verify tail visibility during streaming --------------------------------

echo "==> Checking tail visibility during stream"
TAIL_VISIBLE_COUNT=0

for _ in $(seq 1 30); do
	tmux capture-pane -p -t "$SESSION" >"$OUT"

	if ! tail -n 6 "$OUT" | tr -d '\n' | grep -Fq "Enter send"; then
		fail "composer disappeared from viewport during stream"
	fi

	if grep -Fq "args command:" "$OUT"; then
		TAIL_VISIBLE_COUNT=$((TAIL_VISIBLE_COUNT + 1))
	fi

	if grep -Fq "$FINAL_TEXT" "$OUT"; then
		break
	fi

	sleep 0.25
done

if [[ $TAIL_VISIBLE_COUNT -eq 0 ]]; then
	fail "streamed tail content was never visible in the viewport during streaming"
fi

echo "==> Stream tail was visible in $TAIL_VISIBLE_COUNT/30 polls"

# --- wait for stream to complete -------------------------------------------

echo "==> Waiting for stream to complete"
wait_for "Enter send" 180 || fail "composer did not return to ready after stream"

# --- settle viewport at bottom ---------------------------------------------

echo "==> Settling viewport at bottom"
for _ in $(seq 1 8); do
	send_wheel_down
done
sleep 0.5
wait_for "$FINAL_TEXT" 30 || fail "expected assistant reply text after scroll"

# --- verify final state: latest output near bottom, composer at bottom -----

echo "==> Verifying final viewport state"
tmux capture-pane -p -t "$SESSION" -S -300 >"$OUT"

COMPOSER_LINE="$(grep -n "Enter send" "$OUT" | tail -1 | cut -d: -f1)"
TOTAL_LINES="$(wc -l < "$OUT" | tr -d ' ')"

if [[ -z "$COMPOSER_LINE" ]]; then
	fail "composer hint line not found in final capture"
fi

COMPOSER_FROM_BOTTOM=$((TOTAL_LINES - COMPOSER_LINE))
if [[ $COMPOSER_FROM_BOTTOM -gt 5 ]]; then
	fail "composer is too far from the bottom ($COMPOSER_FROM_BOTTOM lines from bottom)"
fi

if ! grep -Fq "$FINAL_TEXT" "$OUT"; then
	fail "final assistant text not visible in viewport"
fi

FINAL_TEXT_LINE="$(grep -n "$FINAL_TEXT" "$OUT" | tail -1 | cut -d: -f1)"
if [[ "$FINAL_TEXT_LINE" -ge "$COMPOSER_LINE" ]]; then
	fail "final text should appear above the composer"
fi

FINAL_FROM_COMPOSER=$((COMPOSER_LINE - FINAL_TEXT_LINE))
if [[ $FINAL_FROM_COMPOSER -gt 10 ]]; then
	fail "final text is too far from composer ($FINAL_FROM_COMPOSER lines)"
fi

echo "==> Final text is $FINAL_FROM_COMPOSER line(s) above composer"

# --- scroll-away / resume note ---------------------------------------------

echo ""
echo "NOTE: scroll-away/resume behavior is covered by focused unit tests in"
echo "      VirtualHistoryRegion.test.tsx (wheel events + stickyBottom)."
echo "      This smoke only verifies that the live stream keeps the tail visible"
echo "      and the composer pinned during active streaming."
echo ""

echo "==> PASS: stream-tail visibility smoke test succeeded"
echo "--- final pane (redacted) ---"
sed -E 's/(api[_-]?key|Bearer|sk-[A-Za-z0-9_-]+)/[REDACTED]/gi' "$OUT"
