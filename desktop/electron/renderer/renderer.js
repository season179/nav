const messageListNode = document.querySelector("#message-list");
const sessionListNode = document.querySelector("#session-list");
const newChatButton = document.querySelector("#new-chat");
const composer = document.querySelector("#composer");
const input = document.querySelector("#composer-input");
const sendButton = document.querySelector("#composer-send");
const stopButton = document.querySelector("#composer-stop");
const modelNode = document.querySelector("#composer-model");

let connected = false;
let running = false;
let activeSessionId = null;
// Set when Stop is pressed; re-applied on `run.started` in case the press landed
// before the backend had registered the run (when a stop is a no-op).
let stopRequested = false;

// The most recent user/assistant message and its role. Only the last message in
// a consecutive run from one party keeps its timestamp, so when the same party
// speaks again we drop the stamp from its previous turn.
let lastPartyMessage = null;
let lastPartyRole = null;

if (window.nav) {
  window.nav.onBackendStatus(handleBackendStatus);
  window.nav.onSessionEvent(handleSessionEvent);
  newChatButton.addEventListener("click", startNewChat);
} else {
  renderBackendStatus({
    state: "preload-missing",
    message: "Electron preload API unavailable",
  });
}

// Grow the textarea to fit its content so long, multi-line prompts stay
// visible, up to the max-height set in CSS (past which it scrolls).
function autoResizeInput() {
  input.style.height = "auto";
  input.style.height = `${input.scrollHeight}px`;
}

input.addEventListener("input", autoResizeInput);

// Enter sends; Shift+Enter inserts a newline. Ignore Enter while an IME
// composition is active so committing a candidate doesn't send early.
input.addEventListener("keydown", (keyEvent) => {
  if (keyEvent.isComposing) {
    return;
  }
  if (keyEvent.key === "Enter" && !keyEvent.shiftKey) {
    keyEvent.preventDefault();
    composer.requestSubmit();
  }
});

composer.addEventListener("submit", async (submitEvent) => {
  submitEvent.preventDefault();
  const text = input.value.trim();
  if (!text || !connected) {
    return;
  }

  // Sending while a run is in flight steers it: the backend folds the message
  // into the live run at its next model call. Stay in the running state in that
  // case rather than toggling it.
  const wasRunning = running;
  input.value = "";
  autoResizeInput();
  if (!wasRunning) {
    stopRequested = false;
    setRunning(true);
  }
  try {
    await window.nav.sessionSendMessage(text);
  } catch (error) {
    appendMessage("error", `Could not send message: ${error.message}`);
    if (!wasRunning) {
      setRunning(false);
    }
  }
});

stopButton.addEventListener("click", () => {
  stopRun();
});

// Ask the backend to stop the active run. The button is disabled until the run
// actually ends (a `run.cancelled` event), since an in-flight model call must
// finish before the loop can observe the stop.
async function stopRun() {
  if (!window.nav) {
    return;
  }
  stopRequested = true;
  stopButton.disabled = true;
  try {
    await window.nav.sessionStop();
  } catch (error) {
    appendMessage("error", `Could not stop: ${error.message}`);
    stopButton.disabled = false;
  }
}

function handleBackendStatus(status) {
  renderBackendStatus(status);
  if (status.state === "connected") {
    connected = true;
    if (status.sessionId) {
      activeSessionId = status.sessionId;
    }
    setRunning(false);
    refreshSessions();
    refreshModelName();
  }
}

// Show which model the backend is configured to use, just below the composer.
// The model can't change mid-session, so one fetch on connect is enough.
async function refreshModelName() {
  if (!window.nav) {
    return;
  }
  try {
    const info = await window.nav.modelInfo();
    modelNode.textContent = info?.label ?? "";
  } catch {
    // The indicator is best-effort; never let it disrupt the chat.
  }
}

function handleSessionEvent(event) {
  switch (event.type) {
    case "user.message":
      appendMessage("user", event.text);
      break;
    case "run.started":
      setRunning(true);
      // A Stop pressed before the backend registered this run was a no-op; now
      // that the run exists, apply it.
      if (stopRequested) {
        stopRun();
      }
      break;
    case "assistant.tool_calls":
      // The model's reasoning before it calls tools, if it sent any.
      if (event.text) {
        appendMessage("assistant", event.text);
      }
      break;
    case "tool.started":
      upsertToolLine(event.tool_call_id, "running", event.tool_name);
      break;
    case "tool.completed":
      upsertToolLine(event.tool_call_id, "done", event.tool_name, event.text);
      break;
    case "tool.failed":
      upsertToolLine(
        event.tool_call_id,
        "failed",
        event.tool_name,
        event.error,
      );
      break;
    case "message.completed":
      appendMessage("assistant", event.text);
      break;
    case "run.completed":
    case "run.cancelled":
      setRunning(false);
      // A title or recency may have changed; keep the sidebar current.
      refreshSessions();
      break;
    case "run.failed":
      appendMessage("error", event.error ?? "the run failed");
      setRunning(false);
      refreshSessions();
      break;
    default:
      break;
  }
}

// Replace the active conversation with an existing session. The transcript is
// cleared up front; the resumed history streams back in as session events.
async function selectSession(sessionId) {
  if (sessionId === activeSessionId || running || !connected) {
    return;
  }
  // Update optimistically, but remember the previous session so a failed switch
  // can roll the highlight back — otherwise the sidebar and backend disagree on
  // which session is active and the next message routes to the wrong one.
  const previousSessionId = activeSessionId;
  clearTranscript();
  activeSessionId = sessionId;
  markActiveSession();
  try {
    await window.nav.switchSession(sessionId);
  } catch (error) {
    activeSessionId = previousSessionId;
    markActiveSession();
    appendMessage("error", `Could not open session: ${error.message}`);
  }
}

async function startNewChat() {
  if (running || !connected) {
    return;
  }
  clearTranscript();
  try {
    activeSessionId = await window.nav.newSession();
    markActiveSession();
  } catch (error) {
    appendMessage("error", `Could not start a new chat: ${error.message}`);
  }
}

async function refreshSessions() {
  if (!window.nav) {
    return;
  }
  let sessions;
  try {
    sessions = await window.nav.listSessions();
  } catch {
    // Listing is best-effort; never let it disrupt the chat.
    return;
  }

  sessionListNode.replaceChildren();
  if (sessions.length === 0) {
    const empty = document.createElement("li");
    empty.className = "sidebar-empty";
    empty.textContent = "No sessions yet";
    sessionListNode.append(empty);
    return;
  }

  for (const session of sessions) {
    const item = document.createElement("button");
    item.type = "button";
    item.className = "session-item";
    item.dataset.sessionId = session.sessionId;
    item.textContent = sessionTitle(session);
    item.addEventListener("click", () => selectSession(session.sessionId));

    const row = document.createElement("li");
    row.append(item);
    sessionListNode.append(row);
  }

  // One place decides which item is highlighted, for both fresh lists and
  // in-place selection changes.
  markActiveSession();
}

function sessionTitle(session) {
  const title = (session.title ?? "").trim();
  return title.length > 0 ? title : "New chat";
}

function markActiveSession() {
  for (const item of sessionListNode.querySelectorAll(".session-item")) {
    if (item.dataset.sessionId === activeSessionId) {
      item.setAttribute("aria-current", "true");
    } else {
      item.removeAttribute("aria-current");
    }
  }
}

function clearTranscript() {
  messageListNode.replaceChildren();
  lastPartyMessage = null;
  lastPartyRole = null;
}

function appendMessage(role, text) {
  const item = document.createElement("li");
  item.className = `message message-${role}`;

  // Assistant turns are Markdown; everything else (user input, tool output,
  // errors) is plain text and stays verbatim.
  let body;
  if (role === "assistant") {
    body = document.createElement("div");
    body.className = "message-text markdown";
    renderMarkdownInto(body, text);
  } else {
    body = document.createElement("span");
    body.className = "message-text";
    body.textContent = text;
  }
  item.append(body);

  if (role === "user" || role === "assistant") {
    stampLatestTurn(item, role);
  }

  messageListNode.append(item);
  item.scrollIntoView({ block: "end" });
}

// Add a timestamp to a user/assistant turn, keeping it only on the last message
// of a consecutive run from one party: when that party speaks again, drop the
// stamp from its prior turn.
function stampLatestTurn(item, role) {
  if (lastPartyMessage && lastPartyRole === role) {
    lastPartyMessage.querySelector(".message-time")?.remove();
  }

  const now = new Date();
  const time = document.createElement("time");
  time.className = "message-time";
  time.dateTime = now.toISOString();
  time.textContent = formatTimestamp(now);
  item.append(time);

  lastPartyMessage = item;
  lastPartyRole = role;
}

// "dd MMM HH:MM" in 24-hour time, e.g. "31 May 15:46".
function formatTimestamp(date) {
  const day = String(date.getDate()).padStart(2, "0");
  const month = date.toLocaleString("en-US", { month: "short" });
  const hours = String(date.getHours()).padStart(2, "0");
  const minutes = String(date.getMinutes()).padStart(2, "0");
  return `${day} ${month} ${hours}:${minutes}`;
}

// Render a compact tool line — a 🔧 marker, the tool name, and an optional
// truncated preview of its output (or the error on failure). The same call's
// line is reused as it moves from running → done/failed, keyed by its id, so a
// tool shows as one evolving line rather than two separate bubbles.
function upsertToolLine(toolCallId, state, toolName, detail) {
  const selector = toolCallId
    ? `[data-tool-call-id="${CSS.escape(toolCallId)}"]`
    : null;
  let item = selector ? messageListNode.querySelector(selector) : null;
  if (item) {
    item.replaceChildren();
  } else {
    item = document.createElement("li");
    if (toolCallId) {
      item.dataset.toolCallId = toolCallId;
    }
    messageListNode.append(item);
  }
  item.className = `message message-tool message-tool-${state}`;

  const marker = document.createElement("span");
  marker.className = "message-role";
  marker.textContent = toolMarker(state);

  const name = document.createElement("span");
  name.className = "tool-name";
  name.textContent = toolName ?? "tool";

  item.append(marker, name);

  if (detail) {
    const preview = document.createElement("span");
    preview.className = "tool-detail";
    preview.textContent = previewText(detail);
    item.append(preview);
  }

  item.scrollIntoView({ block: "end" });
}

// A terminal-style status glyph for a tool line, keyed by its lifecycle state.
function toolMarker(state) {
  switch (state) {
    case "running":
      return "▸";
    case "failed":
      return "✕";
    default:
      return "●";
  }
}

// Collapse tool output to a short single-line preview so the transcript stays
// readable; the full result still lives in the session history.
function previewText(text) {
  const firstLine = text.split("\n", 1)[0];
  return firstLine.length > 120 ? `${firstLine.slice(0, 117)}…` : firstLine;
}

function setRunning(isRunning) {
  running = isRunning;
  // The composer stays open mid-run so the user can send follow-ups that steer
  // the live run; a separate Stop button appears alongside Send to interrupt it.
  input.disabled = !connected;
  sendButton.disabled = !connected;
  stopButton.hidden = !isRunning;
  stopButton.disabled = !connected;
  // New chat / session switching are blocked mid-run to avoid racing a turn.
  newChatButton.disabled = isRunning || !connected;
  if (connected) {
    input.focus();
  }
}

// With the topbar gone there is nowhere to show ambient state, so only failures
// are surfaced — as transcript errors — while healthy states stay silent.
function renderBackendStatus(status) {
  switch (status.state) {
    case "starting-backend":
    case "connected":
      break;
    case "stream-error":
      appendMessage("error", `Stream error: ${status.message}`);
      break;
    case "backend-error":
      appendMessage("error", `Backend error: ${status.message}`);
      break;
    default:
      appendMessage("error", status.message ?? status.state);
      break;
  }
}

// --- Markdown -------------------------------------------------------------
//
// Assistant turns are Markdown. `marked` parses it to HTML and `DOMPurify`
// sanitizes that HTML before it reaches the DOM — we never hand-build the
// markup. DOMPurify strips scripts, event-handler attributes, and javascript:
// URLs from the untrusted model output, so assigning innerHTML here is safe.
// Both are loaded as UMD globals by index.html.
const marked = window.marked;
const DOMPurify = window.DOMPurify;

// gfm: tables/strikethrough/etc.; breaks: a lone newline becomes <br>, which
// matches how chat text reads.
marked.setOptions({ gfm: true, breaks: true });

// Links must open in the user's browser, not navigate the app window. The hook
// runs on every sanitize pass; noopener keeps the opened page off window.opener.
DOMPurify.addHook("afterSanitizeAttributes", (node) => {
  if (node.tagName === "A") {
    node.setAttribute("target", "_blank");
    node.setAttribute("rel", "noopener noreferrer");
  }
});

// Render `source` Markdown into `element` as sanitized HTML.
function renderMarkdownInto(element, source) {
  element.innerHTML = DOMPurify.sanitize(marked.parse(source));
}
