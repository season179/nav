const http = require("node:http");
const https = require("node:https");

function subscribeToSessionEvents({ backendUrl, sessionId, signal, onEvent, onError }) {
  const eventsUrl = new URL(`/sessions/${sessionId}/events`, backendUrl);
  const transport = eventsUrl.protocol === "https:" ? https : http;
  let buffer = "";
  let closed = false;

  const request = transport.get(eventsUrl, (response) => {
    if (response.statusCode !== 200) {
      response.resume();
      onError(new Error(`SSE request failed with HTTP ${response.statusCode}`));
      return;
    }

    response.setEncoding("utf8");
    response.on("data", (chunk) => {
      buffer += chunk;
      const parsed = parseSseBuffer(buffer);
      buffer = parsed.remainder;
      for (const event of parsed.events) {
        onEvent(event);
      }
    });
    response.on("error", (error) => {
      if (!closed && !signal?.aborted) {
        onError(error);
      }
    });
  });

  request.on("error", (error) => {
    if (!closed && !signal?.aborted) {
      onError(error);
    }
  });

  signal?.addEventListener(
    "abort",
    () => {
      closed = true;
      request.destroy();
    },
    { once: true },
  );

  return {
    close() {
      closed = true;
      request.destroy();
    },
  };
}

function parseSseBuffer(buffer) {
  const events = [];
  const frames = buffer.split(/\n\n/);
  const remainder = frames.pop() ?? "";

  for (const frame of frames) {
    const event = parseSseFrame(frame);
    if (event) {
      events.push(event);
    }
  }

  return { events, remainder };
}

function parseSseFrame(frame) {
  const dataLine = frame
    .split(/\n/)
    .find((line) => line.startsWith("data: "));
  if (!dataLine) {
    return null;
  }

  return JSON.parse(dataLine.slice("data: ".length));
}

module.exports = {
  subscribeToSessionEvents,
};
