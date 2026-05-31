const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { test } = require("node:test");

const {
  createStartupTrace,
  newUuidV7,
  readStartupTraceSetting,
} = require("../desktop/electron/startup-trace.cjs");

test("startup trace is off by default", () => {
  const dir = tempDir("nav-startup-trace-disabled-");
  const traceFile = path.join(dir, "startup.jsonl");
  const trace = createStartupTrace({
    settingsPath: path.join(dir, "missing-settings.json"),
    traceFile,
  });

  assert.equal(trace.enabled, false);
  assert.equal(trace.mark("electron.app.ready"), null);
  assert.deepEqual(trace.childEnv({ NAV_MOCK_MODEL: "1" }), {
    NAV_MOCK_MODEL: "1",
  });
  assert.equal(trace.summaryLine(), null);
  assert.equal(fs.existsSync(traceFile), false);
});

test("startup trace reads the settings toggle", () => {
  assert.equal(
    readStartupTraceSetting({
      readFile: () => JSON.stringify({ observability: { startupTrace: true } }),
    }),
    true,
  );
  assert.equal(
    readStartupTraceSetting({
      readFile: () =>
        JSON.stringify({ observability: { startupTrace: false } }),
    }),
    false,
  );
  assert.equal(
    readStartupTraceSetting({
      readFile: () => JSON.stringify({}),
    }),
    false,
  );
});

test("startup trace writes sanitized JSONL entries and child trace env", () => {
  const dir = tempDir("nav-startup-trace-");
  const traceFile = path.join(dir, "startup.jsonl");
  const trace = createStartupTrace({
    enabled: true,
    nowMs: () => 1_780_000_000_000,
    performanceNow: monotonicClock([10, 12, 15]),
    traceFile,
    traceId: "019e7bb2-7c00-7000-8000-000000000000",
  });

  trace.mark("electron.app.ready", { smoke: true });

  const lines = readJsonl(traceFile);
  assert.equal(lines.length, 2);
  assert.equal(lines[0].event, "startup.trace.created");
  assert.equal(lines[0].source, "electron");
  assert.equal(lines[0].trace_id, trace.traceId);
  assert.equal(lines[1].event, "electron.app.ready");
  assert.equal(lines[1].smoke, true);
  assert.equal(lines[1].elapsed_ms, 5);

  assert.deepEqual(trace.childEnv({ NAV_MOCK_MODEL: "1" }), {
    NAV_MOCK_MODEL: "1",
    NAV_STARTUP_TRACE_ID: trace.traceId,
    NAV_STARTUP_TRACE_STDERR: "1",
  });
});

test("startup trace writes backend events from the Electron writer", () => {
  const dir = tempDir("nav-startup-trace-backend-");
  const traceFile = path.join(dir, "startup.jsonl");
  const trace = createStartupTrace({
    enabled: true,
    performanceNow: monotonicClock([0, 1]),
    traceFile,
    traceId: "019e7bb2-7c00-7000-8000-000000000000",
  });

  const recorded = trace.writeBackendEvent({
    trace_id: trace.traceId,
    source: "backend",
    event: "backend.ready",
    timestamp_ms: 1_780_000_000_000,
    elapsed_ms: 8,
    model_kind: "mock",
  });
  trace.writeBackendEvent({
    trace_id: "019e7bb2-7c00-7000-8000-111111111111",
    event: "backend.ignored",
  });

  assert.equal(recorded.event, "backend.ready");
  const lines = readJsonl(traceFile);
  assert.equal(lines.at(-1).event, "backend.ready");
  assert.equal(
    lines.some((line) => line.event === "backend.ignored"),
    false,
  );
});

test("startup trace rotates the local JSONL file", () => {
  const dir = tempDir("nav-startup-trace-rotate-");
  const traceFile = path.join(dir, "startup.jsonl");
  fs.writeFileSync(traceFile, "old trace\n", "utf8");

  createStartupTrace({
    enabled: true,
    maxBytes: 1,
    maxFiles: 2,
    performanceNow: monotonicClock([0, 1]),
    traceFile,
  });

  assert.equal(fs.readFileSync(`${traceFile}.1`, "utf8"), "old trace\n");
  const freshLines = readJsonl(traceFile);
  assert.equal(freshLines[0].event, "startup.trace.created");
});

test("startup trace ignores unreadable or invalid settings", () => {
  assert.equal(
    readStartupTraceSetting({
      readFile: () => {
        throw Object.assign(new Error("missing"), { code: "ENOENT" });
      },
    }),
    false,
  );
  assert.equal(readStartupTraceSetting({ readFile: () => "not-json" }), false);
});

test("startup trace ids are UUID v7", () => {
  const id = newUuidV7({
    nowMs: () => 1_780_000_000_000,
    randomBytes: () => Buffer.alloc(10, 0xab),
  });

  assert.match(
    id,
    /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
  );
});

function tempDir(prefix) {
  return fs.mkdtempSync(path.join(os.tmpdir(), prefix));
}

function readJsonl(file) {
  return fs
    .readFileSync(file, "utf8")
    .trim()
    .split("\n")
    .map((line) => JSON.parse(line));
}

function monotonicClock(values) {
  const queue = [...values];
  return () => queue.shift() ?? values.at(-1) ?? 0;
}
