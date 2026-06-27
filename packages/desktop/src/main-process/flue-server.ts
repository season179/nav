import { type ChildProcess, spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import net from "node:net";
import path from "node:path";

import type {
  FlueConnection,
  FlueServerStatus,
} from "../lib/flue-connection.js";

type FlueServerOptions = {
  devServerUrl?: string;
  mainProcessDir: string;
  onStatusChange?: (status: FlueServerStatus) => void;
};

const healthTimeoutMs = 40_000;
const healthPollIntervalMs = 250;

export class FlueServer {
  readonly #devServerUrl?: string;
  readonly #flueRoot: string;
  readonly #onStatusChange?: (status: FlueServerStatus) => void;
  readonly #token = randomBytes(32).toString("base64url");

  #baseUrl: string | null = null;
  #child: ChildProcess | null = null;
  #lastOutput = "";
  #startPromise: Promise<FlueConnection> | null = null;
  #status: FlueServerStatus = {
    baseUrl: null,
    message: "Flue has not started yet.",
    pid: null,
    state: "stopped",
  };

  constructor({
    devServerUrl,
    mainProcessDir,
    onStatusChange,
  }: FlueServerOptions) {
    const packageRoot = path.resolve(mainProcessDir, "../..");
    const repoRoot = path.resolve(packageRoot, "../..");

    this.#devServerUrl = devServerUrl;
    this.#flueRoot = path.join(repoRoot, "packages/flue");
    this.#onStatusChange = onStatusChange;
  }

  getStatus() {
    return this.#status;
  }

  async getConnection() {
    return await this.start();
  }

  start() {
    this.#startPromise ??= this.#start().catch((error: unknown) => {
      // Don't cache a failed start; let the next caller retry from scratch.
      this.#startPromise = null;
      throw error;
    });
    return this.#startPromise;
  }

  stop() {
    if (!this.#child || this.#child.killed) {
      this.#setStatus({
        baseUrl: this.#baseUrl,
        message: "Flue is stopped.",
        pid: null,
        state: "stopped",
      });
      return;
    }

    this.#child.kill();
  }

  async #start(): Promise<FlueConnection> {
    const port = await findAvailablePort();
    this.#baseUrl = `http://127.0.0.1:${port}/api`;

    this.#setStatus({
      baseUrl: this.#baseUrl,
      message: "Starting Flue.",
      pid: null,
      state: "starting",
    });

    const child = spawn(
      "pnpm",
      ["--dir", this.#flueRoot, "dev", "--port", String(port)],
      {
        cwd: this.#flueRoot,
        env: {
          ...process.env,
          NAV_DESKTOP_ORIGIN: this.#devServerUrl
            ? new URL(this.#devServerUrl).origin
            : "null",
          NAV_DESKTOP_TOKEN: this.#token,
        },
        shell: process.platform === "win32",
        stdio: ["ignore", "pipe", "pipe"],
      },
    );
    this.#child = child;

    this.#setStatus({
      baseUrl: this.#baseUrl,
      message: "Flue process spawned.",
      pid: child.pid ?? null,
      state: "starting",
    });

    child.stdout?.on("data", (chunk: Buffer) => {
      this.#rememberOutput(chunk);
    });

    child.stderr?.on("data", (chunk: Buffer) => {
      this.#rememberOutput(chunk);
    });

    child.on("exit", (code, signal) => {
      const wasReady = this.#status.state === "ready";

      if (this.#child === child) {
        this.#child = null;
      }

      // A process that became ready and later exited leaves a resolved
      // connection pointing at a now-dead port. Clear the memoized start so the
      // next getConnection() spawns a fresh process instead of handing back the
      // stale one. (Pre-ready exits are cleared by start()'s catch once
      // waitForHealth rejects, so clearing here would risk a double-spawn.)
      if (wasReady) {
        this.#startPromise = null;
      }

      this.#setStatus({
        baseUrl: this.#baseUrl,
        message: wasReady
          ? `Flue stopped (${signal ?? code ?? "unknown"}).`
          : this.#lastOutput ||
            `Flue exited before becoming ready (${signal ?? code ?? "unknown"}).`,
        pid: null,
        state: wasReady ? "stopped" : "failed",
      });
    });

    try {
      await waitForHealth(this.#baseUrl, healthTimeoutMs);
    } catch (error) {
      this.#setStatus({
        baseUrl: this.#baseUrl,
        message:
          error instanceof Error
            ? error.message
            : "Flue failed to become healthy.",
        pid: child.pid ?? null,
        state: "failed",
      });
      throw error;
    }

    this.#setStatus({
      baseUrl: this.#baseUrl,
      message: "Flue is ready.",
      pid: child.pid ?? null,
      state: "ready",
    });

    return {
      baseUrl: this.#baseUrl,
      status: this.#status,
      token: this.#token,
    };
  }

  #rememberOutput(chunk: Buffer) {
    this.#lastOutput = `${this.#lastOutput}${chunk.toString()}`.slice(-2000);
  }

  #setStatus(status: FlueServerStatus) {
    this.#status = status;
    this.#onStatusChange?.(status);
  }
}

const findAvailablePort = async () =>
  await new Promise<number>((resolve, reject) => {
    const server = net.createServer();

    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      server.close(() => {
        if (typeof address === "object" && address?.port) {
          resolve(address.port);
          return;
        }
        reject(new Error("Unable to allocate a local port for Flue."));
      });
    });
  });

const waitForHealth = async (baseUrl: string, timeoutMs: number) => {
  const deadline = Date.now() + timeoutMs;
  const healthUrl = new URL("/health", baseUrl).toString();

  while (Date.now() < deadline) {
    try {
      const response = await fetch(healthUrl);

      if (response.ok) {
        return;
      }
    } catch {
      // Flue is still starting.
    }

    await new Promise((resolve) => setTimeout(resolve, healthPollIntervalMs));
  }

  throw new Error("Timed out waiting for Flue to become healthy.");
};
