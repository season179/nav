const { describe, expect, test } = require("bun:test");

const { createNdjsonParser, unwrapProtocolMessage } = require("./agent-events");

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

  test("unwraps json rpc nav.event notifications", () => {
    const events = [];
    const parser = createNdjsonParser({ onEvent: (event) => events.push(event) });

    parser.push(
      '{"jsonrpc":"2.0","method":"nav.event","params":{"protocol_version":1,"event":{"kind":"turn_complete","usage":{"tokens_input":1}}}}\n',
    );

    expect(events).toEqual([
      { kind: "turn_complete", usage: { tokens_input: 1 } },
    ]);
  });

  test("surfaces json rpc session notifications separately", () => {
    const protocolEvents = [];
    const parser = createNdjsonParser({
      onEvent: () => {},
      onProtocolEvent: (event) => protocolEvents.push(event),
    });

    parser.push(
      '{"jsonrpc":"2.0","method":"nav.session.started","params":{"protocol_version":1,"session_id":"01H","cwd":"/repo","model":"gpt-test","transport":"websocket"}}\n',
    );

    expect(protocolEvents).toEqual([
      {
        method: "nav.session.started",
        params: {
          protocol_version: 1,
          session_id: "01H",
          cwd: "/repo",
          model: "gpt-test",
          transport: "websocket",
        },
      },
    ]);
  });

  test("treats unknown json rpc messages as legacy text", () => {
    expect(
      unwrapProtocolMessage({
        jsonrpc: "2.0",
        method: "nav.unknown",
        params: {},
      }),
    ).toBeNull();
  });
});
