const { describe, expect, test } = require("bun:test");

const { createNdjsonParser } = require("./agent-events");

describe("createNdjsonParser", () => {
  test("parses agent events across chunk boundaries", () => {
    const events = [];
    const parser = createNdjsonParser({ onEvent: (event) => events.push(event) });

    parser.push('{"kind":"assistant_message_delta","text":"hel');
    parser.push('lo"}\n{"kind":"turn_complete"');
    parser.push(',"usage":{"tokens_input":0}}\n');

    expect(events).toEqual([
      { kind: "assistant_message_delta", text: "hello" },
      { kind: "turn_complete", usage: { tokens_input: 0 } },
    ]);
  });

  test("forwards non-json lines as legacy text", () => {
    const text = [];
    const parser = createNdjsonParser({
      onEvent: () => {},
      onText: (chunk) => text.push(chunk),
    });

    parser.push("plain output\n");

    expect(text).toEqual(["plain output\n"]);
  });
});
