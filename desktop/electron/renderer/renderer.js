const statusNode = document.querySelector("#backend-status");
const sessionNode = document.querySelector("#session-id");
const messageListNode = document.querySelector("#message-list");
const composer = document.querySelector("#composer");
const input = document.querySelector("#composer-input");
const sendButton = document.querySelector("#composer-send");

let connected = false;
let running = false;

if (window.nav) {
  window.nav.onBackendStatus(handleBackendStatus);
  window.nav.onSessionEvent(handleSessionEvent);
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
    setRunning(false);
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
      break;
    case "run.failed":
      appendMessage("error", event.error ?? "the run failed");
      setRunning(false);
      break;
    default:
      break;
  }
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
