#!/usr/bin/env bash
# Record Chat Completions SSE fixtures from a local Ollama or vLLM server.
#
# Usage:
#   ./record.sh [MODEL] [BASE_URL]
#
# Defaults:
#   MODEL    = llama3.2
#   BASE_URL = http://localhost:11434   (Ollama default)
#
# For vLLM:
#   ./record.sh meta-llama/Llama-3.2-1B http://localhost:8000
#
# Prerequisites:
#   - curl, jq
#   - A running Ollama/vLLM server with the model loaded
#   - For tool-call fixtures, the model must support tool calling
#     (e.g. llama3.2, mistral, qwen2.5).

set -euo pipefail

MODEL="${1:-llama3.2}"
BASE_URL="${2:-http://localhost:11434}"
DIR="$(cd "$(dirname "$0")" && pwd)"
URL="${BASE_URL}/v1/chat/completions"

# ── Tool schemas (shared across fixtures) ─────────────────────

TOOL_READ_FILE='{"type":"function","function":{"name":"read_file","description":"Read a file","parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}}}'
TOOL_BASH='{"type":"function","function":{"name":"bash","description":"Run a shell command","parameters":{"type":"object","properties":{"command":{"type":"string"}},"required":["command"]}}}'
TOOLS_BOTH="[${TOOL_READ_FILE},${TOOL_BASH}]"

# ── Helpers ────────────────────────────────────────────────────

stream() {
  local name="$1" body="$2"
  local out="${DIR}/${name}.sse"
  echo "→ Recording ${name}.sse ..."
  curl -sf -N "${URL}" \
    -H "Content-Type: application/json" \
    -d "${body}" \
    -o "${out}"
  echo "  ✓ ${out} ($(wc -l < "${out}") lines)"
}

sidecar() {
  local name="$1" description="$2"
  local out="${DIR}/${name}.json"
  python3 "${DIR}/parse_sse.py" "${DIR}/${name}.sse" "${description}" > "${out}"
  echo "  ✓ ${out}"
}

record() {
  local name="$1" description="$2" body="$3"
  stream "${name}" "${body}"
  sidecar "${name}" "${description}"
}

# ── Fixtures ───────────────────────────────────────────────────

record "text_only" \
  "Pure text response with no tool calls. Content arrives as delta.content chunks." \
  "$(jq -nc --arg m "$MODEL" '{
    model:$m, stream:true, stream_options:{include_usage:true},
    messages:[{role:"user",content:"Say hello in one short sentence."}]
  }')"

record "single_tool_call" \
  "Single tool call with arguments streamed in chunks. No assistant text before the call." \
  "$(jq -nc --arg m "$MODEL" --argjson tools "[${TOOL_READ_FILE}]" '{
    model:$m, stream:true, stream_options:{include_usage:true},
    messages:[{role:"user",content:"Read the file main.rs"}],
    tools:$tools
  }')"

record "parallel_tool_calls" \
  "Two parallel tool calls interleaved by index. Tests that the accumulator correctly reassembles arguments across interleaving deltas." \
  "$(jq -nc --arg m "$MODEL" --argjson tools "${TOOLS_BOTH}" '{
    model:$m, stream:true, stream_options:{include_usage:true},
    messages:[{role:"user",content:"Read a.rs and run ls. Do both at once."}],
    tools:$tools
  }')"

record "text_then_tool_call" \
  "Assistant emits text content first, then makes a single tool call. Tests the content-to-tool-call transition." \
  "$(jq -nc --arg m "$MODEL" --argjson tools "[${TOOL_READ_FILE}]" '{
    model:$m, stream:true, stream_options:{include_usage:true},
    messages:[{role:"user",content:"What does the project config look like? Read config.toml for me."}],
    tools:$tools
  }')"

# 5. Context overflow — send an impossibly long prompt.
#    Hard to trigger reliably; the fixture is typically hand-crafted.
#    If the server returns a non-200 (most do for overflow), we skip.
echo "→ Attempting context_overflow_error.sse ..."
OVERFLOW_BODY=$(jq -nc --arg m "$MODEL" --arg big "$(python3 -c "print('x ' * 100000)")" '{
  model:$m, stream:true,
  messages:[{role:"user",content:$big}]
}')
if stream "context_overflow_error" "${OVERFLOW_BODY}"; then
  # Only generate sidecar if we got a real SSE response.
  if grep -q "data:" "${DIR}/context_overflow_error.sse"; then
    sidecar "context_overflow_error" \
      "Context window overflow. The server returns an error object with code context_length_exceeded."
  else
    echo "  ⚠ Response was not SSE; keeping hand-crafted fixture."
  fi
else
  echo "  ⚠ Server rejected the request; keeping hand-crafted fixture."
fi

echo ""
echo "Done. Verify fixtures and sidecars in ${DIR}/"
