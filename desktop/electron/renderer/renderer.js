const statusNode = document.querySelector("#backend-status");
const sessionNode = document.querySelector("#session-id");
const messageListNode = document.querySelector("#message-list");
const sessionListNode = document.querySelector("#session-list");
const newChatButton = document.querySelector("#new-chat");
const composer = document.querySelector("#composer");
const input = document.querySelector("#composer-input");
const sendButton = document.querySelector("#composer-send");

let connected = false;
let running = false;
let activeSessionId = null;

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

composer.addEventListener("submit", async (submitEvent) => {
  submitEvent.preventDefault();
  const text = input.value.trim();
  if (!text || running || !connected) {
    return;
  }

  input.value = "";
  setRunning(true);
  try {
    await window.nav.sessionSendMessage(text);
  } catch (error) {
    appendMessage("error", `Could not send message: ${error.message}`);
    setRunning(false);
  }
});

function handleBackendStatus(status) {
  renderBackendStatus(status);
  if (status.state === "connected") {
    connected = true;
    if (status.sessionId) {
      activeSessionId = status.sessionId;
    }
    setRunning(false);
    refreshSessions();
  }
}

function handleSessionEvent(event) {
  switch (event.type) {
    case "user.message":
      appendMessage("user", event.text);
      break;
    case "run.started":
      setRunning(true);
      break;
    case "message.completed":
      appendMessage("assistant", event.text);
      break;
    case "run.completed":
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
}

function appendMessage(role, text) {
  const item = document.createElement("li");
  item.className = `message message-${role}`;

  const who = document.createElement("span");
  who.className = "message-role";
  who.textContent = roleLabel(role);

  const body = document.createElement("span");
  body.className = "message-text";
  body.textContent = text;

  item.append(who, body);
  messageListNode.append(item);
  item.scrollIntoView({ block: "end" });
}

function roleLabel(role) {
  switch (role) {
    case "user":
      return "You";
    case "assistant":
      return "nav";
    default:
      return "error";
  }
}

function setRunning(isRunning) {
  running = isRunning;
  const disabled = isRunning || !connected;
  input.disabled = disabled;
  sendButton.disabled = disabled;
  // New chat / session switching are blocked mid-run to avoid racing a turn.
  newChatButton.disabled = disabled;
  if (!disabled) {
    input.focus();
  }
}

function renderBackendStatus(status) {
  statusNode.textContent = formatStatus(status);
  statusNode.dataset.state = status.state;
  if (status.sessionId) {
    sessionNode.textContent = `Session ${status.sessionId}`;
  }
}

function formatStatus(status) {
  switch (status.state) {
    case "starting-backend":
      return "Starting backend";
    case "connected":
      return "Connected";
    case "stream-error":
      return `Stream error: ${status.message}`;
    case "backend-error":
      return `Backend error: ${status.message}`;
    default:
      return status.message ?? status.state;
  }
}
