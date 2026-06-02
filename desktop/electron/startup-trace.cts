import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { performance } from "node:perf_hooks";

const DEFAULT_TRACE_FILE = path.join(
  os.homedir(),
  ".nav",
  "traces",
  "startup.jsonl",
);
const DEFAULT_MAX_BYTES = 1024 * 1024;
const DEFAULT_MAX_FILES = 5;

type TraceEntry = {
  trace_id: string;
  source: string;
  event: string;
  timestamp_ms: number;
  elapsed_ms: number;
  [key: string]: unknown;
};

type ReadFileFn = (path: string, encoding: BufferEncoding) => string;
type AppendLineFn = (traceFile: string, line: string) => void;
type RotateFn = (
  traceFile: string,
  options: { maxBytes?: number; maxFiles?: number },
) => void;

export type StartupTraceOptions = {
  enabled?: boolean;
  readFile?: ReadFileFn;
  settingsPath?: string;
  appendLine?: AppendLineFn;
  maxBytes?: number;
  maxFiles?: number;
  nowMs?: () => number;
  performanceNow?: () => number;
  pid?: number;
  rotate?: RotateFn;
  traceFile?: string;
  traceId?: string;
};

// The public surface shared by the enabled and no-op traces. The main process
// and backend process only depend on this shape.
export type StartupTrace = {
  enabled: boolean;
  mark(event: string, fields?: Record<string, unknown>): TraceEntry | null;
  childEnv(overrides?: Record<string, string>): Record<string, string>;
  writeBackendEvent(entry: unknown): TraceEntry | null;
  summaryLine(): string | null;
};

export function createStartupTrace(
  options: StartupTraceOptions & { enabled: true },
): EnabledStartupTrace;
export function createStartupTrace(options?: StartupTraceOptions): StartupTrace;
export function createStartupTrace(
  options: StartupTraceOptions = {},
): StartupTrace {
  const enabled = options.enabled ?? readStartupTraceSetting(options);
  if (!enabled) {
    return new NoopStartupTrace();
  }
  return new EnabledStartupTrace(options);
}

class NoopStartupTrace implements StartupTrace {
  enabled = false;

  mark(): null {
    return null;
  }

  childEnv(overrides: Record<string, string> = {}): Record<string, string> {
    return { ...overrides };
  }

  writeBackendEvent(): null {
    return null;
  }

  summaryLine(): null {
    return null;
  }
}

export class EnabledStartupTrace implements StartupTrace {
  enabled = true;
  events: TraceEntry[] = [];
  traceId: string;
  traceFile: string;
  writeFailed = false;

  private appendLine: AppendLineFn;
  private nowMs: () => number;
  private performanceNow: () => number;
  private startMs: number;

  constructor({
    appendLine = appendJsonLine,
    maxBytes = DEFAULT_MAX_BYTES,
    maxFiles = DEFAULT_MAX_FILES,
    nowMs = () => Date.now(),
    performanceNow = () => performance.now(),
    pid = process.pid,
    rotate = rotateTraceFile,
    traceFile = DEFAULT_TRACE_FILE,
    traceId = newUuidV7(),
  }: StartupTraceOptions = {}) {
    this.appendLine = appendLine;
    this.nowMs = nowMs;
    this.performanceNow = performanceNow;
    this.startMs = performanceNow();
    this.traceFile = traceFile;
    this.traceId = traceId;

    try {
      rotate(traceFile, { maxBytes, maxFiles });
    } catch {
      this.writeFailed = true;
    }

    this.mark("startup.trace.created", { pid });
  }

  mark(event: string, fields: Record<string, unknown> = {}): TraceEntry {
    const entry: TraceEntry = {
      trace_id: this.traceId,
      source: "electron",
      event,
      timestamp_ms: this.nowMs(),
      elapsed_ms: roundMs(this.performanceNow() - this.startMs),
      ...fields,
    };
    this.events.push(entry);
    this.write(entry);
    return entry;
  }

  childEnv(overrides: Record<string, string> = {}): Record<string, string> {
    return {
      ...overrides,
      NAV_STARTUP_TRACE_ID: this.traceId,
      NAV_STARTUP_TRACE_STDERR: "1",
    };
  }

  writeBackendEvent(entry: unknown): TraceEntry | null {
    const candidate = entry as { trace_id?: unknown } | null | undefined;
    if (!candidate || candidate.trace_id !== this.traceId) {
      return null;
    }
    const backendEntry = {
      ...(candidate as Record<string, unknown>),
      trace_id: this.traceId,
      source: "backend",
    } as TraceEntry;
    this.write(backendEntry);
    return backendEntry;
  }

  duration(startEvent: string, endEvent: string): number | null {
    const start = this.events.find((entry) => entry.event === startEvent);
    const end = [...this.events]
      .reverse()
      .find((entry) => entry.event === endEvent);
    if (!start || !end || end.elapsed_ms < start.elapsed_ms) {
      return null;
    }
    return roundMs(end.elapsed_ms - start.elapsed_ms);
  }

  summaryLine(): string {
    const connectedAt = this.events.find(
      (entry) => entry.event === "electron.connected",
    );
    const rendererLoadedAt = this.events.find(
      (entry) => entry.event === "electron.renderer.did_finish_load",
    );
    const totalMs =
      maxMs(connectedAt?.elapsed_ms, rendererLoadedAt?.elapsed_ms) ??
      this.latestElapsedMs();
    const backendMs = this.duration(
      "electron.backend.spawn.start",
      "electron.backend.ready",
    );
    const rendererMs = this.duration(
      "electron.window.create.start",
      "electron.renderer.did_finish_load",
    );
    const sessionMs = this.duration(
      "electron.session.open.start",
      "electron.session.open.end",
    );

    return [
      `nav electron startup: total=${formatMs(totalMs)}`,
      `backend=${formatMs(backendMs)}`,
      `session=${formatMs(sessionMs)}`,
      `renderer=${formatMs(rendererMs)}`,
      `trace=${this.traceFile}`,
    ].join(" ");
  }

  latestElapsedMs(): number {
    return this.events.at(-1)?.elapsed_ms ?? 0;
  }

  write(entry: TraceEntry): void {
    if (this.writeFailed) {
      return;
    }
    try {
      this.appendLine(this.traceFile, `${JSON.stringify(entry)}\n`);
    } catch {
      this.writeFailed = true;
    }
  }
}

export function readStartupTraceSetting({
  readFile = fs.readFileSync as ReadFileFn,
  settingsPath = path.join(os.homedir(), ".nav", "settings.json"),
}: {
  readFile?: ReadFileFn;
  settingsPath?: string;
} = {}): boolean {
  try {
    const settings = JSON.parse(readFile(settingsPath, "utf8"));
    return settings?.observability?.startupTrace === true;
  } catch {
    return false;
  }
}

function appendJsonLine(traceFile: string, line: string): void {
  fs.mkdirSync(path.dirname(traceFile), { recursive: true });
  fs.appendFileSync(traceFile, line, "utf8");
}

export function rotateTraceFile(
  traceFile: string,
  {
    maxBytes = DEFAULT_MAX_BYTES,
    maxFiles = DEFAULT_MAX_FILES,
  }: { maxBytes?: number; maxFiles?: number } = {},
): void {
  if (maxBytes <= 0 || maxFiles <= 0) {
    return;
  }

  let stat: fs.Stats;
  try {
    stat = fs.statSync(traceFile);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") {
      return;
    }
    throw error;
  }

  if (stat.size < maxBytes) {
    return;
  }

  fs.rmSync(`${traceFile}.${maxFiles}`, { force: true });
  for (let index = maxFiles - 1; index >= 1; index -= 1) {
    const from = `${traceFile}.${index}`;
    const to = `${traceFile}.${index + 1}`;
    if (fs.existsSync(from)) {
      fs.renameSync(from, to);
    }
  }
  fs.renameSync(traceFile, `${traceFile}.1`);
}

export function newUuidV7({
  nowMs = () => Date.now(),
  randomBytes = crypto.randomBytes,
}: {
  nowMs?: () => number;
  randomBytes?: (size: number) => Buffer;
} = {}): string {
  const timestamp = BigInt(nowMs());
  const random = randomBytes(10);
  const bytes = Buffer.alloc(16);

  bytes[0] = Number((timestamp >> 40n) & 0xffn);
  bytes[1] = Number((timestamp >> 32n) & 0xffn);
  bytes[2] = Number((timestamp >> 24n) & 0xffn);
  bytes[3] = Number((timestamp >> 16n) & 0xffn);
  bytes[4] = Number((timestamp >> 8n) & 0xffn);
  bytes[5] = Number(timestamp & 0xffn);
  bytes[6] = 0x70 | (random[0] & 0x0f);
  bytes[7] = random[1];
  bytes[8] = 0x80 | (random[2] & 0x3f);
  for (let index = 9; index < 16; index += 1) {
    bytes[index] = random[index - 6];
  }

  return [
    bytes.toString("hex", 0, 4),
    bytes.toString("hex", 4, 6),
    bytes.toString("hex", 6, 8),
    bytes.toString("hex", 8, 10),
    bytes.toString("hex", 10, 16),
  ].join("-");
}

function roundMs(value: number): number {
  return Math.round(value * 100) / 100;
}

function formatMs(value: number | null | undefined): string {
  return value === null || value === undefined
    ? "n/a"
    : `${Math.round(value)}ms`;
}

function maxMs(...values: Array<number | null | undefined>): number | null {
  const numericValues = values.filter(
    (value): value is number => typeof value === "number",
  );
  return numericValues.length > 0 ? Math.max(...numericValues) : null;
}
