const statusNode = document.querySelector("#backend-status");
const sessionNode = document.querySelector("#session-id");
const eventListNode = document.querySelector("#event-list");

const events = [];

if (window.nav) {
  window.nav.onBackendStatus(renderBackendStatus);
  window.nav.onSessionEvent((event) => {
    events.push(event);
    renderEvents();
  });
} else {
  renderBackendStatus({
    state: "preload-missing",
    message: "Electron preload API unavailable",
  });
}

function renderBackendStatus(status) {
  statusNode.textContent = formatStatus(status);
  statusNode.dataset.state = status.state;

  if (status.sessionId) {
    sessionNode.textContent = status.sessionId;
  }
}

function renderEvents() {
  eventListNode.replaceChildren(
    ...events.map((event) => {
      const row = document.createElement("li");
      row.className = "event-row";

      const type = document.createElement("span");
      type.className = "event-type";
      type.textContent = event.type;

      const summary = document.createElement("span");
      summary.className = "event-summary";
      summary.textContent = summarizeEvent(event);

      row.append(type, summary);
      return row;
    }),
  );
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

function summarizeEvent(event) {
  if (event.text) {
    return event.text;
  }

  if (event.finish_reason) {
    return `finish_reason=${event.finish_reason}`;
  }

  if (event.status) {
    return `status=${event.status}`;
  }

  if (event.run_id) {
    return event.run_id;
  }

  return event.event_id;
}
