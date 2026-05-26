#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
TUI="$ROOT/tui"
SESSION="nav-ink-smoke"
TMP="$(mktemp -d)"
NAV_BACKEND="$ROOT/target/debug/nav-backend"

cleanup() {
	tmux kill-session -t "$SESSION" 2>/dev/null || true
	pkill -f "$NAV_BACKEND serve-http" 2>/dev/null || true
	rm -rf "$TMP"
}
trap cleanup EXIT

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

echo "==> Launching Ink TUI in tmux (${SESSION}, 80x24)"
tmux kill-session -t "$SESSION" 2>/dev/null || true
tmux new-session -d -s "$SESSION" -x 80 -y 24 \
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

echo "==> Sending a prompt (real LLM)"
tmux set-buffer -b nav-smoke 'Say exactly: ink-tmux-ok'
tmux paste-buffer -b nav-smoke -t "$SESSION" -p
sleep 0.2
tmux send-keys -t "$SESSION" Enter

wait_for "ink-tmux-ok" 30 || fail "user message did not appear in history"
wait_for "Running" 30 || true
wait_gone "Running" 180 || fail "run did not finish within timeout"
wait_for "Enter send" 30 || fail "composer did not return to ready after run"
wait_for "ink-tmux-ok" 10 || fail "expected assistant reply text"

tmux capture-pane -p -t "$SESSION" -S -300 >"$OUT"
USER_LINE="$(grep -n "ink-tmux-ok" "$OUT" | head -1 | cut -d: -f1 || true)"
if [[ -z "$USER_LINE" ]]; then
	fail "expected user message in history"
fi
if [[ "$USER_LINE" -ge "$COMPOSER_LINE" ]]; then
	fail "user message should stay above the composer"
fi
if grep -Fq "Connection failed" "$OUT" || grep -Fq "backend exited" "$OUT"; then
	fail "TUI reported a connection/backend error"
fi

# Require assistant reply text after the user message.
if ! awk -v user="$USER_LINE" -v comp="$COMPOSER_LINE" 'NR > user && NR < comp && length($0) > 0 { found=1 } END { exit !found }' "$OUT"; then
	fail "expected assistant text between user message and composer"
fi

echo "==> PASS: tmux smoke test succeeded"
echo "--- final pane (redacted) ---"
sed -E 's/(api[_-]?key|Bearer|sk-[A-Za-z0-9_-]+)/[REDACTED]/gi' "$OUT"
