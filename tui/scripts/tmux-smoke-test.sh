#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
TUI="$ROOT/tui"
SESSION="nav-ink-smoke"
TMP="$(mktemp -d)"
REAL_NAV_BACKEND="$ROOT/target/debug/nav-backend"
E2E_BACKEND="$TUI/scripts/nav-e2e-backend.ts"
TMUX_WIDTH=120
TMUX_HEIGHT=53
NAV_E2E="${NAV_E2E:-1}"
COMMANDS="$TMP/commands.txt"

cleanup() {
	tmux kill-session -t "$SESSION" 2>/dev/null || true
	pkill -f "$REAL_NAV_BACKEND serve-http" 2>/dev/null || true
	pkill -f "$E2E_BACKEND serve-http" 2>/dev/null || true
	rm -rf "$TMP"
}
trap cleanup EXIT

bun run "$E2E_BACKEND" print-commands >"$COMMANDS"
FINAL_TEXT_VALUE="$(bun run "$E2E_BACKEND" print-final-text)"
WHEEL_REVEALED_VALUE="$(bun run "$E2E_BACKEND" print-wheel-revealed)"

send_wheel_up() {
	tmux send-keys -t "$SESSION" -H 1b 5b 3c 36 34 3b 31 3b 31 30 4d
}

send_wheel_down() {
	tmux send-keys -t "$SESSION" -H 1b 5b 3c 36 35 3b 31 3b 31 30 4d
}

if [[ "$NAV_E2E" == "1" ]]; then
	echo "==> Using deterministic NAV_E2E backend"
	NAV_BACKEND="$E2E_BACKEND"
else
	echo "==> Building nav-backend"
	cargo build --quiet --manifest-path "$ROOT/Cargo.toml" -p nav-backend
	if [[ -n "${NAV_MODEL_SETTINGS:-}" ]]; then
		echo "==> Using NAV_MODEL_SETTINGS=$NAV_MODEL_SETTINGS"
	elif [[ -f "${HOME}/.nav/settings.json" ]]; then
		echo "==> Using ~/.nav/settings.json"
	else
		echo "FAIL: no model settings found (set NAV_MODEL_SETTINGS or create ~/.nav/settings.json)" >&2
		exit 1
	fi
	NAV_BACKEND="$REAL_NAV_BACKEND"
fi

echo "==> Checking backend bootstrap"
BOOTSTRAP="$(
	timeout 5 "$NAV_BACKEND" serve-http 2>&1 | head -1 || true
)"
pkill -f "$NAV_BACKEND serve-http" 2>/dev/null || true
sleep 0.5
if [[ "$BOOTSTRAP" != *'"type":"backend.ready"'* && "$BOOTSTRAP" != *'"type": "backend.ready"'* ]]; then
	echo "FAIL: nav-backend did not print backend.ready on stdout" >&2
	echo "$BOOTSTRAP" >&2
	exit 1
fi

pkill -f "$NAV_BACKEND serve-http" 2>/dev/null || true
sleep 0.3

echo "==> Launching Ink TUI in tmux (${SESSION}, ${TMUX_WIDTH}x${TMUX_HEIGHT})"
tmux kill-session -t "$SESSION" 2>/dev/null || true
tmux new-session -d -s "$SESSION" -x "$TMUX_WIDTH" -y "$TMUX_HEIGHT" \
	"cd \"$TUI\" && exec env NAV_BACKEND=\"$NAV_BACKEND\" bun run start"
sleep 1

OUT="$TMP/pane.txt"
fail() {
	echo "FAIL: $1"
	echo "--- tmux pane ---"
	cat "$OUT" || true
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

wait_gone() {
	local needle="$1"
	local attempts="${2:-40}"
	for _ in $(seq 1 "$attempts"); do
		tmux capture-pane -p -t "$SESSION" -S -300 >"$OUT" || true
		if ! grep -Fq "$needle" "$OUT"; then
			return 0
		fi
		sleep 0.25
	done
	return 1
}

echo "==> Waiting for connection"
wait_for "/model" 60 || fail "composer did not reach ready state"

echo "==> Checking layout (history top, composer bottom)"
tmux capture-pane -p -t "$SESSION" -S -300 >"$OUT"
COMPOSER_LINE="$(grep -n "Enter send" "$OUT" | tail -1 | cut -d: -f1)"
HINT_LINE="$(grep -n "Ask a question" "$OUT" | head -1 | cut -d: -f1 || true)"
if [[ -z "$COMPOSER_LINE" ]]; then
	fail "could not find composer hint line"
fi
if [[ -n "$HINT_LINE" && "$HINT_LINE" -ge "$COMPOSER_LINE" ]]; then
	fail "welcome text should appear above the composer"
fi
if ! tail -n 6 "$OUT" | tr -d '\n' | grep -Fq "Enter send"; then
	fail "composer should be visible at the bottom of the terminal"
fi
PROMPT_LINE="$(grep -n '^>' "$OUT" | tail -1 | cut -d: -f1 || true)"
if [[ -z "$PROMPT_LINE" ]]; then
	fail "composer input row (> prompt) not visible"
fi
if [[ "$PROMPT_LINE" -ge "$COMPOSER_LINE" ]]; then
	fail "input row should appear above the hint line"
fi

echo "==> Typing into composer (send-keys)"
tmux send-keys -t "$SESSION" 'x'
sleep 0.4
tmux capture-pane -p -t "$SESSION" -S -30 >"$OUT"
if ! grep -Fq 'x' "$OUT"; then
	fail "typed character did not appear in composer (keyboard input broken)"
fi
tmux send-keys -t "$SESSION" Escape
sleep 0.2

echo "==> Sending residue prompt"
if [[ "$NAV_E2E" == "1" ]]; then
	tmux set-buffer -b nav-smoke 'Run the deterministic tmux residue smoke.'
else
	REAL_PROMPT="Run each command from this list with the bash tool, one at a time, then say exactly: $FINAL_TEXT_VALUE. Commands: $(paste -sd ';' "$COMMANDS")"
	tmux set-buffer -b nav-smoke "$REAL_PROMPT"
fi
tmux paste-buffer -b nav-smoke -t "$SESSION" -p
sleep 0.2
tmux send-keys -t "$SESSION" Enter

wait_for "Running" 30 || true
wait_gone "Running" 180 || fail "run did not finish within timeout"
wait_for "Enter send" 30 || fail "composer did not return to ready after run"

echo "==> Settling viewport at bottom"
for _ in $(seq 1 8); do
	send_wheel_down
done
sleep 0.5
wait_for "$FINAL_TEXT_VALUE" 30 || fail "expected assistant reply text"

tmux capture-pane -p -t "$SESSION" -S -300 >"$OUT"
COMPOSER_LINE="$(grep -n "Enter send" "$OUT" | tail -1 | cut -d: -f1)"
if grep -Fq "Connection failed" "$OUT" || grep -Fq "backend exited" "$OUT"; then
	fail "TUI reported a connection/backend error"
fi

if ! awk -v comp="$COMPOSER_LINE" -v final="$FINAL_TEXT_VALUE" 'index($0, final) { if (NR < comp) found=1 } END { exit !found }' "$OUT"; then
	fail "expected assistant text above the composer"
fi

tmux capture-pane -p -t "$SESSION" -S -500 >"$OUT"

echo "==> Injecting SGR wheel-up events"
WHEEL_BEFORE="$TMP/wheel-before.txt"
WHEEL_AFTER="$TMP/wheel-after.txt"
tmux capture-pane -p -t "$SESSION" >"$WHEEL_BEFORE"
for _ in $(seq 1 8); do
	send_wheel_up
done
sleep 0.5
tmux capture-pane -p -t "$SESSION" >"$WHEEL_AFTER"

echo "==> Checking residue detector"
PREDICTED_ROWS="$(tmux display-message -p -t "$SESSION" '#{pane_height}')"
bun run "$TUI/scripts/tmux-residue-detector.ts" \
	--capture "$OUT" \
	--commands "$COMMANDS" \
	--final-text "$FINAL_TEXT_VALUE" \
	--predicted-rows "$PREDICTED_ROWS" \
	--wheel-before "$WHEEL_BEFORE" \
	--wheel-after "$WHEEL_AFTER" \
	--wheel-revealed "$WHEEL_REVEALED_VALUE"

echo "==> PASS: tmux smoke test succeeded"
echo "--- final pane (redacted) ---"
sed -E 's/(api[_-]?key|Bearer|sk-[A-Za-z0-9_-]+)/[REDACTED]/gi' "$OUT"
