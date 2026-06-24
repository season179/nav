import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import { dirname } from "node:path";
import { observe } from "@flue/runtime";
import { createUuidV7 } from "./ids.js";

export type StackRequest = {
  api: string;
  url: string;
  model: string;
  body?: unknown;
};

export type StackResponse = {
  statusCode?: number;
  body?: unknown;
  error?: string;
  tokenUsage?: unknown;
};

export type StackEntry = {
  id: string;
  runId: string;
  sequence: number;
  status: string;
  startedAtMs: number;
  durationMs: number;
  request?: StackRequest;
  response?: StackResponse;
};

export type SessionStacksResult = {
  stacks: StackEntry[];
  unavailableReason?: string;
};

type StackFile = {
  version: 1;
  sessions: Record<string, StackEntry[]>;
};

type ObservationLike = Record<string, unknown> & {
  instanceId?: unknown;
  operationId?: unknown;
  turnId?: unknown;
  type?: unknown;
  timestamp?: unknown;
};

export class StackStore {
  readonly #filePath: string;
  readonly #clock: () => number;

  constructor({
    filePath,
    clock = Date.now,
  }: {
    filePath: string;
    clock?: () => number;
  }) {
    this.#filePath = filePath;
    this.#clock = clock;
  }

  async list(sessionId: string): Promise<SessionStacksResult> {
    const data = await this.load();
    return {
      stacks: [...(data.sessions[sessionId] ?? [])].toSorted(
        (left, right) => left.sequence - right.sequence,
      ),
    };
  }

  async deleteSession(sessionId: string): Promise<void> {
    const data = await this.load();
    delete data.sessions[sessionId];
    await this.save(data);
  }

  async recordObservation(observation: unknown): Promise<void> {
    const event = toObservation(observation);
    if (!event) {
      return;
    }

    if (event.type === "turn_request") {
      await this.recordTurnRequest(event);
      return;
    }

    if (event.type === "turn") {
      await this.recordTurn(event);
    }
  }

  private async recordTurnRequest(event: ObservationLike): Promise<void> {
    const sessionId = stringValue(event.instanceId);
    const turnId = stringValue(event.turnId);
    if (!sessionId || !turnId) {
      return;
    }

    const data = await this.load();
    const sessionStacks = data.sessions[sessionId] ?? [];
    const existing = sessionStacks.find((entry) => entry.id === turnId);
    const startedAtMs = timestampMs(event.timestamp, this.#clock);

    if (existing) {
      existing.request = buildRequest(event.request);
      existing.startedAtMs = startedAtMs;
    } else {
      sessionStacks.push({
        id: turnId,
        runId: stringValue(event.operationId) ?? turnId,
        sequence: nextSequence(sessionStacks),
        status: "pending",
        startedAtMs,
        durationMs: 0,
        request: buildRequest(event.request),
      });
    }

    data.sessions[sessionId] = sessionStacks;
    await this.save(data);
  }

  private async recordTurn(event: ObservationLike): Promise<void> {
    const sessionId = stringValue(event.instanceId);
    const turnId = stringValue(event.turnId);
    if (!sessionId || !turnId) {
      return;
    }

    const data = await this.load();
    const sessionStacks = data.sessions[sessionId] ?? [];
    const existing = sessionStacks.find((entry) => entry.id === turnId);
    const isError = event.isError === true;
    const response = buildResponse(event.response, isError);
    const durationMs = numberValue(event.durationMs) ?? 0;

    if (existing) {
      existing.status = isError ? "failed" : "completed";
      existing.durationMs = durationMs;
      existing.response = response;
      existing.request ??= buildRequest(event.request);
    } else {
      sessionStacks.push({
        id: turnId,
        runId: stringValue(event.operationId) ?? turnId,
        sequence: nextSequence(sessionStacks),
        status: isError ? "failed" : "completed",
        startedAtMs: timestampMs(event.timestamp, this.#clock),
        durationMs,
        request: buildRequest(event.request),
        response,
      });
    }

    data.sessions[sessionId] = sessionStacks;
    await this.save(data);
  }

  private async load(): Promise<StackFile> {
    try {
      const raw = await readFile(this.#filePath, "utf8");
      const parsed = JSON.parse(raw) as Partial<StackFile>;
      return {
        version: 1,
        sessions:
          parsed.sessions && typeof parsed.sessions === "object"
            ? parsed.sessions
            : {},
      };
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") {
        return { version: 1, sessions: {} };
      }

      throw error;
    }
  }

  private async save(data: StackFile): Promise<void> {
    await mkdir(dirname(this.#filePath), { recursive: true });
    const tempPath = `${this.#filePath}.${process.pid}.${createUuidV7()}.tmp`;
    await writeFile(tempPath, `${JSON.stringify(data, null, 2)}\n`);
    await rename(tempPath, this.#filePath);
  }
}

export function startStackObservation(store: StackStore): () => void {
  return observe((observation) => {
    void store.recordObservation(observation).catch((error: unknown) => {
      console.warn("failed to record stack observation", error);
    });
  });
}

function buildRequest(request: unknown): StackRequest {
  const record = asRecord(request);
  const provider =
    stringValue(record.providerName) ??
    stringValue(record.providerId) ??
    "unknown";

  return {
    api: provider,
    url: stringValue(record.url) ?? `flue://${provider}`,
    model:
      stringValue(record.model) ?? stringValue(record.modelId) ?? "unknown",
    body: sanitizeJson(record.input ?? record.body ?? request),
  };
}

function buildResponse(response: unknown, isError: boolean): StackResponse {
  const record = asRecord(response);
  const error =
    stringValue(record.error) ??
    stringValue(asRecord(record.normalizedError).message) ??
    (isError ? "model turn failed" : undefined);

  return {
    body: sanitizeJson(record.output ?? response),
    error,
    tokenUsage: sanitizeJson(record.usage),
  };
}

function toObservation(observation: unknown): ObservationLike | null {
  if (!observation || typeof observation !== "object") {
    return null;
  }

  return observation as ObservationLike;
}

function asRecord(value: unknown): Record<string, unknown> {
  return value && typeof value === "object"
    ? (value as Record<string, unknown>)
    : {};
}

function sanitizeJson(value: unknown): unknown {
  if (value === undefined) {
    return undefined;
  }

  try {
    const serialized = JSON.stringify(value, (_key, nestedValue) =>
      nestedValue instanceof Uint8Array ? "[binary data omitted]" : nestedValue,
    );
    return serialized === undefined ? undefined : JSON.parse(serialized);
  } catch {
    return "[unserializable value omitted]";
  }
}

function nextSequence(entries: StackEntry[]): number {
  return entries.reduce((max, entry) => Math.max(max, entry.sequence), 0) + 1;
}

function timestampMs(timestamp: unknown, fallbackClock: () => number): number {
  if (typeof timestamp !== "string") {
    return fallbackClock();
  }

  const parsed = Date.parse(timestamp);
  return Number.isFinite(parsed) ? parsed : fallbackClock();
}

function stringValue(value: unknown): string | undefined {
  return typeof value === "string" && value.length > 0 ? value : undefined;
}

function numberValue(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value)
    ? value
    : undefined;
}
