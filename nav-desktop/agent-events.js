const unwrapProtocolMessage = (message) => {
  if (!message || typeof message !== "object") {
    return null;
  }

  if (message.jsonrpc === "2.0" && message.method === "nav.event") {
    return { type: "agent_event", event: message.params?.event };
  }

  if (message.jsonrpc === "2.0" && message.method === "nav.session.started") {
    return {
      type: "protocol_event",
      method: message.method,
      params: message.params,
    };
  }

  if (typeof message.kind === "string") {
    return { type: "agent_event", event: message };
  }

  return null;
};

const createNdjsonParser = ({ onEvent, onProtocolEvent, onText }) => {
  let buffer = "";

  const pushLine = (line) => {
    const trimmed = line.trim();
    if (!trimmed) {
      return;
    }
    try {
      const parsed = JSON.parse(trimmed);
      const message = unwrapProtocolMessage(parsed);
      if (message?.type === "agent_event" && message.event) {
        onEvent(message.event);
      } else if (message?.type === "protocol_event") {
        onProtocolEvent?.({ method: message.method, params: message.params });
      } else {
        onText?.(`${line}\n`);
      }
    } catch {
      onText?.(`${line}\n`);
    }
  };

  return {
    push(chunk) {
      buffer += chunk;
      const lines = buffer.split(/\r?\n/);
      buffer = lines.pop() ?? "";
      lines.forEach(pushLine);
    },
    flush() {
      if (buffer) {
        pushLine(buffer);
        buffer = "";
      }
    },
  };
};

module.exports = { createNdjsonParser, unwrapProtocolMessage };
