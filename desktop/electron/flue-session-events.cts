import type { SessionEvent } from "./types.cjs";

export type FlueStreamControl = {
  streamNextOffset?: string;
  streamCursor?: string;
  upToDate?: boolean;
  streamClosed?: boolean;
};

export type FlueSseParseResult = {
  events: SessionEvent[];
  controls: FlueStreamControl[];
  remainder: string;
  malformedFrames: number;
};

type FlueEventRecord = Record<string, unknown>;

export function parseFlueSseBuffer(
  buffer: string,
  options: { sessionId?: string } = {},
): FlueSseParseResult {
  const events: SessionEvent[] = [];
  const controls: FlueStreamControl[] = [];
  let malformedFrames = 0;
  const frames = buffer.replace(/\r\n/g, "\n").split(/\n\n/);
  const remainder = frames.pop() ?? "";

  for (const frame of frames) {
    const parsed = parseSseFrame(frame);
    if (!parsed) {
      continue;
    }

    const payload = parseJson(parsed.data);
    if (payload === undefined) {
      malformedFrames += 1;
      continue;
    }

    if (parsed.eventName === "control") {
      const control = toControl(payload);
      if (control) {
        controls.push(control);
      } else {
        malformedFrames += 1;
      }
      continue;
    }

    const batch = Array.isArray(payload) ? payload : [payload];
    const mapped = flueEventsToSessionEvents(batch, options);
    if (mapped.length === 0 && batch.length > 0) {
      malformedFrames += countMalformedEvents(batch);
    }
    events.push(...mapped);
  }

  return { events, controls, remainder, malformedFrames };
}

export function flueEventsToSessionEvents(
  flueEvents: readonly unknown[],
  options: { sessionId?: string } = {},
): SessionEvent[] {
  const events: SessionEvent[] = [];
  const failedSessions = new Set<string>();

  for (const flueEvent of flueEvents) {
    for (const event of flueEventToSessionEvents(flueEvent, options)) {
      const sessionKey = event.session_id ?? options.sessionId ?? "";
      if (event.type === "run.completed" && failedSessions.has(sessionKey)) {
        continue;
      }
      if (event.type === "run.failed") {
        failedSessions.add(sessionKey);
      }
      events.push(event);
    }
  }

  return events;
}

export function flueEventToSessionEvents(
  flueEvent: unknown,
  options: { sessionId?: string } = {},
): SessionEvent[] {
  const event = asRecord(flueEvent);
  const type = stringValue(event.type);
  if (!type) {
    return [];
  }

  switch (type) {
    case "agent_start":
      return [navEvent(event, "run.started", options)];
    case "text_delta": {
      const text =
        stringValue(event.text) ??
        stringValue(event.delta) ??
        stringValue(event.content);
      return text ? [navEvent(event, "message.delta", options, { text })] : [];
    }
    case "message_end":
      return mapMessageEnd(event, options);
    case "tool_start":
      return [
        navEvent(event, "tool.started", options, {
          tool_call_id: toolCallId(event),
          tool_name: toolName(event),
        }),
      ];
    case "tool":
      return [mapToolEnd(event, options)];
    case "turn":
    case "operation":
    case "agent_end":
      return isErrorEvent(event)
        ? [
            navEvent(event, "run.failed", options, {
              error: errorText(event) ?? "the run failed",
            }),
          ]
        : [];
    case "idle":
      return [navEvent(event, "run.completed", options)];
    case "submission_settled":
      return stringValue(event.outcome) === "failed"
        ? [
            navEvent(event, "run.failed", options, {
              error: errorText(event) ?? "the run failed",
            }),
          ]
        : [navEvent(event, "run.completed", options)];
    default:
      return [];
  }
}

function parseSseFrame(
  frame: string,
): { eventName: string; data: string } | null {
  let eventName = "message";
  const data: string[] = [];

  for (const rawLine of frame.split(/\n/)) {
    if (!rawLine || rawLine.startsWith(":")) {
      continue;
    }
    const separator = rawLine.indexOf(":");
    const field = separator === -1 ? rawLine : rawLine.slice(0, separator);
    let value = separator === -1 ? "" : rawLine.slice(separator + 1);
    if (value.startsWith(" ")) {
      value = value.slice(1);
    }

    if (field === "event") {
      eventName = value || "message";
    } else if (field === "data") {
      data.push(value);
    }
  }

  if (data.length === 0) {
    return null;
  }

  return { eventName, data: data.join("\n") };
}

function parseJson(data: string): unknown {
  try {
    return JSON.parse(data);
  } catch {
    return undefined;
  }
}

function mapMessageEnd(
  event: FlueEventRecord,
  options: { sessionId?: string },
): SessionEvent[] {
  const message = asRecord(event.message);
  const role = stringValue(event.role) ?? stringValue(message.role);
  const text = messageText(event);

  if (role === "user") {
    return [navEvent(event, "user.message", options, { text })];
  }
  if (role === "assistant") {
    return text
      ? [navEvent(event, "message.completed", options, { text })]
      : [];
  }
  return [];
}

function mapToolEnd(
  event: FlueEventRecord,
  options: { sessionId?: string },
): SessionEvent {
  if (isErrorEvent(event)) {
    return navEvent(event, "tool.failed", options, {
      tool_call_id: toolCallId(event),
      tool_name: toolName(event),
      error: errorText(event) ?? "tool failed",
    });
  }

  return navEvent(event, "tool.completed", options, {
    tool_call_id: toolCallId(event),
    tool_name: toolName(event),
    text: displayText(event.result ?? event.output ?? event.response),
  });
}

function navEvent(
  event: FlueEventRecord,
  type: string,
  options: { sessionId?: string },
  extra: Partial<SessionEvent> = {},
): SessionEvent {
  const sessionId = stringValue(event.instanceId) ?? options.sessionId;
  const sequence = numberValue(event.eventIndex);
  const eventId =
    sequence === undefined
      ? undefined
      : `flue:${sessionId ?? "unknown"}:${sequence}:${type}`;

  return {
    type,
    ...(eventId ? { event_id: eventId } : {}),
    ...(sessionId ? { session_id: sessionId } : {}),
    ...(sequence === undefined ? {} : { sequence }),
    ...((stringValue(event.operationId) ?? stringValue(event.dispatchId))
      ? {
          run_id:
            stringValue(event.operationId) ?? stringValue(event.dispatchId),
        }
      : {}),
    ...(stringValue(event.messageId) ? { message_id: event.messageId } : {}),
    ...extra,
  };
}

function toControl(payload: unknown): FlueStreamControl | null {
  const record = asRecord(payload);
  if (Object.keys(record).length === 0) {
    return null;
  }

  return {
    ...(stringValue(record.streamNextOffset)
      ? { streamNextOffset: stringValue(record.streamNextOffset) }
      : {}),
    ...(stringValue(record.streamCursor)
      ? { streamCursor: stringValue(record.streamCursor) }
      : {}),
    ...(typeof record.upToDate === "boolean"
      ? { upToDate: record.upToDate }
      : {}),
    ...(typeof record.streamClosed === "boolean"
      ? { streamClosed: record.streamClosed }
      : {}),
  };
}

function countMalformedEvents(events: readonly unknown[]): number {
  return events.filter((event) => !stringValue(asRecord(event).type)).length;
}

function messageText(event: FlueEventRecord): string | undefined {
  const message = asRecord(event.message);
  return (
    stringValue(event.text) ??
    contentText(event.content) ??
    contentText(message.content) ??
    stringValue(message.text)
  );
}

function contentText(value: unknown): string | undefined {
  if (typeof value === "string") {
    return value;
  }

  if (Array.isArray(value)) {
    const text = value
      .map((entry) => contentText(entry))
      .filter((entry): entry is string => typeof entry === "string")
      .join("");
    return text.length > 0 ? text : undefined;
  }

  const record = asRecord(value);
  if (Object.keys(record).length === 0) {
    return undefined;
  }

  return (
    stringValue(record.text) ??
    stringValue(record.content) ??
    contentText(record.value)
  );
}

function isErrorEvent(event: FlueEventRecord): boolean {
  return (
    event.isError === true ||
    stringValue(event.status) === "failed" ||
    event.error !== undefined ||
    event.normalizedError !== undefined
  );
}

function errorText(event: FlueEventRecord): string | undefined {
  return displayText(
    event.error ?? asRecord(event.normalizedError).message ?? event.result,
  );
}

function toolCallId(event: FlueEventRecord): string | undefined {
  return (
    stringValue(event.toolCallId) ??
    stringValue(event.tool_call_id) ??
    stringValue(event.callId) ??
    stringValue(event.id)
  );
}

function toolName(event: FlueEventRecord): string | undefined {
  return (
    stringValue(event.toolName) ??
    stringValue(event.tool_name) ??
    stringValue(event.name) ??
    stringValue(event.tool)
  );
}

function displayText(value: unknown): string | undefined {
  if (value === undefined) {
    return undefined;
  }
  if (typeof value === "string") {
    return value;
  }

  const message = stringValue(asRecord(value).message);
  if (message) {
    return message;
  }

  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}

function asRecord(value: unknown): FlueEventRecord {
  return value && typeof value === "object" ? (value as FlueEventRecord) : {};
}

function stringValue(value: unknown): string | undefined {
  return typeof value === "string" && value.length > 0 ? value : undefined;
}

function numberValue(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value)
    ? value
    : undefined;
}
