import { type ChildProcess, spawn } from "node:child_process";
import { setTimeout as delay } from "node:timers/promises";
import type { StartupTrace } from "./startup-trace.cjs";

const STARTUP_PREFIX = "nav local backend listening on ";
const STARTUP_TRACE_PREFIX = "nav startup trace ";
const DEFAULT_STARTUP_ATTEMPTS = 1200;
const STARTUP_POLL_MS = 50;
const MAX_OUTPUT_LINES = 12;

type StartLocalBackendOptions = {
  projectRoot: string;
  startupAttempts?: number;
  env?: NodeJS.ProcessEnv;
  trace?: StartupTrace;
};

export async function startLocalBackend({
  projectRoot,
  startupAttempts = DEFAULT_STARTUP_ATTEMPTS,
  env = {},
  trace,
}: StartLocalBackendOptions): Promise<{ child: ChildProcess; url: string }> {
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

  const stdoutStream = child.stdout;
  const stderrStream = child.stderr;
  if (!stdoutStream || !stderrStream) {
    throw new Error("backend process was spawned without stdio pipes");
  }

  let stdout = "";
  let stderr = "";
  let stderrRemainder = "";
  const handleStderrLine = (line: string): void => {
    if (!line.startsWith(STARTUP_TRACE_PREFIX)) {
      stderr += `${line}\n`;
      return;
    }

    try {
      trace?.writeBackendEvent(
        JSON.parse(line.slice(STARTUP_TRACE_PREFIX.length)),
      );
    } catch {
      stderr += `${line}\n`;
    }
  };
  stdoutStream.on("data", (chunk: Buffer) => {
    stdout += chunk.toString("utf8");
  });
  stderrStream.on("data", (chunk: Buffer) => {
    stderrRemainder = collectStderrLines({
      chunk: chunk.toString("utf8"),
      previousRemainder: stderrRemainder,
      onLine: handleStderrLine,
    });
  });
  stderrStream.on("end", () => {
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

    await delay(STARTUP_POLL_MS);
  }

  child.kill();
  trace?.mark("electron.backend.startup.timeout");
  throw new Error(
    buildStartupTimeoutMessage({
      startupAttempts,
      stdout,
      stderr,
    }),
  );
}

export function findBackendUrl(stdout: string): string | null {
  const line = stdout
    .split(/\r?\n/)
    .find((entry) => entry.startsWith(STARTUP_PREFIX));

  return line?.replace(STARTUP_PREFIX, "") ?? null;
}

export function collectStderrLines({
  chunk,
  previousRemainder = "",
  onLine,
}: {
  chunk: string;
  previousRemainder?: string;
  onLine: (line: string) => void;
}): string {
  const lines = `${previousRemainder}${chunk}`.split(/\r?\n/);
  const remainder = lines.pop() ?? "";
  for (const line of lines) {
    onLine(line);
  }
  return remainder;
}

export function buildStartupTimeoutMessage({
  startupAttempts,
  stdout = "",
  stderr = "",
}: {
  startupAttempts: number;
  stdout?: string;
  stderr?: string;
}): string {
  const seconds = formatTimeoutSeconds(startupAttempts * STARTUP_POLL_MS);
  const details: string[] = [];
  const stderrSummary = summarizeOutput(stderr);
  const stdoutSummary = summarizeOutput(stdout);

  if (stderrSummary) {
    details.push(`backend stderr:\n${stderrSummary}`);
  }
  if (stdoutSummary) {
    details.push(`backend stdout:\n${stdoutSummary}`);
  }

  const message = `backend did not print a local URL within ${seconds}s`;
  return details.length > 0 ? `${message}\n${details.join("\n")}` : message;
}

function summarizeOutput(output: string): string {
  return output
    .trim()
    .split(/\r?\n/)
    .filter(Boolean)
    .slice(-MAX_OUTPUT_LINES)
    .join("\n");
}

function formatTimeoutSeconds(timeoutMs: number): string {
  return (Math.ceil(timeoutMs / 100) / 10).toFixed(1);
}
