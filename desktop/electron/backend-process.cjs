const { spawn } = require("node:child_process");
const { setTimeout: delay } = require("node:timers/promises");

const STARTUP_PREFIX = "nav local backend listening on ";
const STARTUP_TRACE_PREFIX = "nav startup trace ";

async function startLocalBackend({
  projectRoot,
  startupAttempts = 80,
  env = {},
  trace,
}) {
  trace?.mark("electron.backend.spawn.start");
  const child = spawn(
    "cargo",
    [
      "run",
      "--quiet",
      "--bin",
      "nav-local-backend",
      "--",
      "--bind",
      "127.0.0.1:0",
    ],
    {
      cwd: projectRoot,
      stdio: ["ignore", "pipe", "pipe"],
      // Inherit the user's environment (so a real NAV_API_KEY flows through),
      // with caller overrides such as forcing the mock model.
      env: { ...process.env, ...env },
    },
  );
  trace?.mark("electron.backend.process.spawned", { pid: child.pid });

  let stdout = "";
  let stderr = "";
  let stderrRemainder = "";
  const handleStderrLine = (line) => {
    if (!line.startsWith(STARTUP_TRACE_PREFIX)) {
      stderr += `${line}\n`;
      return;
    }

    try {
      trace?.writeBackendEvent(
        JSON.parse(line.slice(STARTUP_TRACE_PREFIX.length)),
      );
    } catch (_error) {
      stderr += `${line}\n`;
    }
  };
  child.stdout.on("data", (chunk) => {
    stdout += chunk.toString("utf8");
  });
  child.stderr.on("data", (chunk) => {
    stderrRemainder = collectStderrLines({
      chunk: chunk.toString("utf8"),
      previousRemainder: stderrRemainder,
      onLine: handleStderrLine,
    });
  });
  child.stderr.on("end", () => {
    if (stderrRemainder) {
      handleStderrLine(stderrRemainder);
      stderrRemainder = "";
    }
  });

  for (let attempt = 0; attempt < startupAttempts; attempt += 1) {
    const url = findBackendUrl(stdout);
    if (url) {
      trace?.mark("electron.backend.url.detected", { attempt });
      return { child, url };
    }

    if (child.exitCode !== null) {
      trace?.mark("electron.backend.exited_before_startup", {
        exit_code: child.exitCode,
      });
      throw new Error(`backend exited before startup: ${stderr.trim()}`);
    }

    await delay(50);
  }

  child.kill();
  trace?.mark("electron.backend.startup.timeout");
  throw new Error("backend did not print a local URL");
}

function findBackendUrl(stdout) {
  const line = stdout
    .split(/\r?\n/)
    .find((entry) => entry.startsWith(STARTUP_PREFIX));

  return line?.replace(STARTUP_PREFIX, "") ?? null;
}

function collectStderrLines({ chunk, previousRemainder = "", onLine }) {
  const lines = `${previousRemainder}${chunk}`.split(/\r?\n/);
  const remainder = lines.pop() ?? "";
  for (const line of lines) {
    onLine(line);
  }
  return remainder;
}

module.exports = {
  collectStderrLines,
  findBackendUrl,
  startLocalBackend,
};
