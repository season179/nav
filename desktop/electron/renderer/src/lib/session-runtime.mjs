// Per-session transcript and run state for the multi-session renderer.
//
// Several chat sessions can be live at once: each keeps its own transcript and
// run status so a background session's events never bleed into the one on
// screen. These helpers are pure (no React, no Electron) so that isolation can
// be unit tested by replaying events for different sessions and asserting they
// stay independent.

// A fresh, idle session: empty transcript, not running, no model metadata yet.
// `messageSeq` numbers messages within this session only, so ids never collide
// with another session's list even though every session counts from zero.
export function createSessionState() {
  return {
    messages: [],
    running: false,
    stopPending: false,
    modelInfo: null,
    stackAvailable: false,
    stackRefreshKey: 0,
    messageSeq: 0,
  };
}

// Append a chat bubble (user, assistant, or error), advancing the session's
// own message counter so the React key is stable and unique within the list.
export function appendMessage(state, role, text) {
  const messageSeq = state.messageSeq + 1;
  return {
    ...state,
    messageSeq,
    messages: [
      ...state.messages,
      {
        id: `message-${messageSeq}`,
        role,
        text: text ?? "",
        createdAt: new Date().toISOString(),
      },
    ],
  };
}

// Open or update a tool line. A tool call's lifecycle (running -> done/failed)
// reuses one line keyed by `toolCallId`, so a later event rewrites the existing
// line in place instead of appending a duplicate.
export function upsertToolLine(state, toolCallId, toolState, toolName, detail) {
  const makeLine = (existingId, messageSeq) => ({
    id: existingId ?? `message-${messageSeq}`,
    role: "tool",
    toolCallId,
    state: toolState,
    toolName: toolName ?? "tool",
    detail: detail ?? "",
  });

  const index = toolCallId
    ? state.messages.findIndex(
        (message) =>
          message.role === "tool" && message.toolCallId === toolCallId,
      )
    : -1;

  if (index === -1) {
    const messageSeq = state.messageSeq + 1;
    return {
      ...state,
      messageSeq,
      messages: [...state.messages, makeLine(null, messageSeq)],
    };
  }

  const messages = state.messages.slice();
  messages[index] = makeLine(messages[index].id, state.messageSeq);
  return { ...state, messages };
}

// Run lifecycle events that end a run. The renderer also refreshes session
// metadata on these, but the state effect (clear running + any pending stop) is
// shared, so it lives here.
export const TERMINAL_RUN_EVENTS = new Set([
  "run.completed",
  "run.cancelled",
  "run.failed",
]);

export function isTerminalRunEvent(eventType) {
  return TERMINAL_RUN_EVENTS.has(eventType);
}

// Advance one session's state for a single event. Only the transcript and the
// running/stopPending flags are derived here; side effects (model info, stack
// refreshes) stay in the renderer. Returns the same object reference when the
// event changes nothing, so callers can skip a re-render.
export function reduceSessionState(state, event) {
  switch (event.type) {
    case "user.message":
      return appendMessage(state, "user", event.text);
    case "run.started":
      return state.running ? state : { ...state, running: true };
    case "assistant.tool_calls":
      return event.text ? appendMessage(state, "assistant", event.text) : state;
    case "tool.started":
      return upsertToolLine(
        state,
        event.tool_call_id,
        "running",
        event.tool_name,
      );
    case "tool.completed":
      return upsertToolLine(
        state,
        event.tool_call_id,
        "done",
        event.tool_name,
        event.text,
      );
    case "tool.failed":
      return upsertToolLine(
        state,
        event.tool_call_id,
        "failed",
        event.tool_name,
        event.error,
      );
    case "message.completed":
      return appendMessage(state, "assistant", event.text);
    case "run.completed":
    case "run.cancelled":
      return finishRun(state);
    case "run.failed":
      return finishRun(
        appendMessage(state, "error", event.error ?? "the run failed"),
      );
    default:
      return state;
  }
}

function finishRun(state) {
  if (!state.running && !state.stopPending) {
    return state;
  }
  return { ...state, running: false, stopPending: false };
}

// Apply an event to a map of sessions keyed by id, touching only the session it
// names. This is the core of "sessions do not interfere": an event for session
// A returns a map in which every other session is the exact same object it was
// before. Events without a session id (or for an unrecognized type) are no-ops.
export function reduceSessions(states, event) {
  const sessionId = event?.session_id;
  if (!sessionId) {
    return states;
  }
  const current = states[sessionId] ?? createSessionState();
  const next = reduceSessionState(current, event);
  // Nothing changed (an unrecognized or empty event): leave the map untouched,
  // so a no-op event never inserts a phantom empty session.
  if (next === current) {
    return states;
  }
  return { ...states, [sessionId]: next };
}
