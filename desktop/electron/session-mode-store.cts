import fs from "node:fs";
import type { SessionMode } from "./request-validation.cjs";

// Persisted preference for the kind of session nav opens: "local" stays in the
// checkout, "worktree" runs inside a managed git worktree. Stored as small JSON
// in Electron `userData` so the main process can read it at startup — before the
// renderer (and its in-memory composer state) exists. Defaults to "local"
// whenever the file is missing, unreadable, or malformed: a corrupt preference
// must never block launch.

export const DEFAULT_SESSION_MODE: SessionMode = "local";

// Coerce any stored/incoming value to a known mode, falling back to the default
// rather than throwing — this is the read/persist path, where a corrupt or
// unexpected value must degrade quietly. (The IPC trust boundary uses the
// throwing `normalizeSessionMode` in preload/request-validation instead.)
export function coerceSessionMode(value: unknown): SessionMode {
  return value === "worktree" ? "worktree" : DEFAULT_SESSION_MODE;
}

export function readSessionMode(filePath: string): SessionMode {
  try {
    const parsed = JSON.parse(fs.readFileSync(filePath, "utf8"));
    return coerceSessionMode(parsed?.newSessionMode);
  } catch {
    return DEFAULT_SESSION_MODE;
  }
}

// A single small writeFileSync is effectively one write for a payload this size,
// and `readSessionMode` already falls back to the default on a torn/corrupt
// file, so no temp-file dance is needed.
export function writeSessionMode(filePath: string, mode: unknown): SessionMode {
  const normalized = coerceSessionMode(mode);
  fs.writeFileSync(
    filePath,
    `${JSON.stringify({ newSessionMode: normalized })}\n`,
  );
  return normalized;
}
