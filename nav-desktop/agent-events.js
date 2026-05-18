const createNdjsonParser = ({ onEvent, onText }) => {
  let buffer = "";

  const pushLine = (line) => {
    const trimmed = line.trim();
    if (!trimmed) {
      return;
    }
    try {
      onEvent(JSON.parse(trimmed));
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

module.exports = { createNdjsonParser };
