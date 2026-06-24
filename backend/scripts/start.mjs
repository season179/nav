import { spawn } from "node:child_process";
import { createServer } from "node:net";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const STARTUP_PREFIX = "[flue] Server listening on ";
const NAV_STARTUP_PREFIX = "nav local backend listening on ";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const backendRoot = resolve(scriptDir, "..");
const serverPath = resolve(backendRoot, "dist/server.mjs");
const { port } = await resolvePort(process.argv.slice(2), process.env);

const child = spawn(process.execPath, [serverPath], {
  cwd: backendRoot,
  env: { ...process.env, PORT: String(port) },
  stdio: ["ignore", "pipe", "pipe"],
});

let announced = false;
let stdoutRemainder = "";

child.stdout?.on("data", (chunk) => {
  stdoutRemainder = emitStdoutLines(`${stdoutRemainder}${chunk}`);
});

child.stdout?.on("end", () => {
  if (stdoutRemainder) {
    emitStdoutLine(stdoutRemainder);
    stdoutRemainder = "";
  }
});

child.stderr?.pipe(process.stderr, { end: false });

child.on("error", (error) => {
  console.error(`failed to start nav backend: ${error.message}`);
  process.exitCode = 1;
});

child.on("exit", (code, signal) => {
  if (signal) {
    process.kill(process.pid, signal);
    return;
  }

  process.exit(code ?? 1);
});

for (const signal of ["SIGINT", "SIGTERM"]) {
  process.on(signal, () => {
    child.kill(signal);
  });
}

function emitStdoutLines(buffer) {
  const lines = buffer.split(/\r?\n/);
  const remainder = lines.pop() ?? "";
  for (const line of lines) {
    emitStdoutLine(line);
  }
  return remainder;
}

function emitStdoutLine(line) {
  console.log(line);

  if (!announced && line.startsWith(STARTUP_PREFIX)) {
    announced = true;
    console.log(`${NAV_STARTUP_PREFIX}http://127.0.0.1:${port}`);
  }
}

async function resolvePort(args, env) {
  const requested = readPortArg(args) ?? env.PORT ?? "0";
  const numericPort = Number.parseInt(requested, 10);

  if (
    !Number.isInteger(numericPort) ||
    numericPort < 0 ||
    numericPort > 65535
  ) {
    throw new Error(`invalid backend port: ${requested}`);
  }

  if (numericPort !== 0) {
    return { port: numericPort };
  }

  return { port: await getAvailablePort() };
}

function readPortArg(args) {
  const portIndex = args.indexOf("--port");
  if (portIndex >= 0) {
    return args[portIndex + 1];
  }

  const prefixed = args.find((arg) => arg.startsWith("--port="));
  return prefixed?.slice("--port=".length);
}

function getAvailablePort() {
  return new Promise((resolvePort, reject) => {
    const server = createServer();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      const portNumber =
        typeof address === "object" && address ? address.port : undefined;
      server.close((error) => {
        if (error) {
          reject(error);
          return;
        }

        if (typeof portNumber !== "number") {
          reject(new Error("failed to reserve a backend port"));
          return;
        }

        resolvePort(portNumber);
      });
    });
  });
}
