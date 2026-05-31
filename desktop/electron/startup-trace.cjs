const crypto = require("node:crypto");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { performance } = require("node:perf_hooks");

const DEFAULT_TRACE_FILE = path.join(
  os.homedir(),
  ".nav",
  "traces",
  "startup.jsonl",
);
const DEFAULT_MAX_BYTES = 1024 * 1024;
const DEFAULT_MAX_FILES = 5;

function createStartupTrace(options = {}) {
  const enabled = options.enabled ?? readStartupTraceSetting(options);
  if (!enabled) {
    return new NoopStartupTrace(options);
  }
  return new StartupTrace(options);
}

class NoopStartupTrace {
  constructor() {
    this.enabled = false;
  }

  mark() {
    return null;
  }

  childEnv(overrides = {}) {
    return { ...overrides };
  }

  summaryLine() {
    return null;
  }
}

class StartupTrace {
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
  } = {}) {
    this.appendLine = appendLine;
    this.enabled = true;
    this.events = [];
    this.nowMs = nowMs;
    this.performanceNow = performanceNow;
    this.startMs = performanceNow();
    this.traceFile = traceFile;
    this.traceId = traceId;
    this.writeFailed = false;

    try {
      rotate(traceFile, { maxBytes, maxFiles });
    } catch (_error) {
      this.writeFailed = true;
    }

    this.mark("startup.trace.created", { pid });
  }

  mark(event, fields = {}) {
    const entry = {
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

  childEnv(overrides = {}) {
    return {
      ...overrides,
      NAV_STARTUP_TRACE_ID: this.traceId,
      NAV_STARTUP_TRACE_PATH: this.traceFile,
    };
  }

  duration(startEvent, endEvent) {
    const start = this.events.find((entry) => entry.event === startEvent);
    const end = [...this.events]
      .reverse()
      .find((entry) => entry.event === endEvent);
    if (!start || !end || end.elapsed_ms < start.elapsed_ms) {
      return null;
    }
    return roundMs(end.elapsed_ms - start.elapsed_ms);
  }

  summaryLine() {
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

  latestElapsedMs() {
    return this.events.at(-1)?.elapsed_ms ?? 0;
  }

  write(entry) {
    if (this.writeFailed) {
      return;
    }
    try {
      this.appendLine(this.traceFile, `${JSON.stringify(entry)}\n`);
    } catch (_error) {
      this.writeFailed = true;
    }
  }
}

function readStartupTraceSetting({
  readFile = fs.readFileSync,
  settingsPath = path.join(os.homedir(), ".nav", "settings.json"),
} = {}) {
  try {
    const settings = JSON.parse(readFile(settingsPath, "utf8"));
    return settings?.observability?.startupTrace === true;
  } catch (_error) {
    return false;
  }
}

function appendJsonLine(traceFile, line) {
  fs.mkdirSync(path.dirname(traceFile), { recursive: true });
  fs.appendFileSync(traceFile, line, "utf8");
}

function rotateTraceFile(
  traceFile,
  { maxBytes = DEFAULT_MAX_BYTES, maxFiles = DEFAULT_MAX_FILES } = {},
) {
  if (maxBytes <= 0 || maxFiles <= 0) {
    return;
  }

  let stat;
  try {
    stat = fs.statSync(traceFile);
  } catch (error) {
    if (error.code === "ENOENT") {
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

function newUuidV7({
  nowMs = () => Date.now(),
  randomBytes = crypto.randomBytes,
} = {}) {
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

function roundMs(value) {
  return Math.round(value * 100) / 100;
}

function formatMs(value) {
  return value === null || value === undefined
    ? "n/a"
    : `${Math.round(value)}ms`;
}

function maxMs(...values) {
  const numericValues = values.filter((value) => typeof value === "number");
  return numericValues.length > 0 ? Math.max(...numericValues) : null;
}

module.exports = {
  createStartupTrace,
  newUuidV7,
  readStartupTraceSetting,
  rotateTraceFile,
};
