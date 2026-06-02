/**
 * reset-data — wipe nav's local runtime data for a clean-slate dev session.
 *
 * Removes the SQLite session database (nav.db + -wal/-shm), the stacks log,
 * and the blobs/ and traces/ artifact directories. nav recreates an empty
 * schema on its next launch.
 *
 * Safety rails (this is meant to be safe to run as a developer):
 *   - Never touches settings.json, its *.bak backups, or nav.log.
 *   - Refuses to run while a nav-local-backend process is alive (the DB is
 *     open in WAL mode; wiping it live risks corruption). Override with --force.
 *   - Moves data into a recoverable trash dir by default instead of deleting.
 *     Restore by moving it back; reclaim the space with --purge or by deleting
 *     the trash dir. Use --purge to delete immediately.
 *   - Prompts for confirmation unless -y/--yes.
 *   - Honors NAV_DB_PATH and NAV_STACKS_PATH.
 *
 * Usage: bun run reset-data [--purge] [--keep-blobs] [--dry-run] [-y] [--force]
 */

import { spawnSync } from "node:child_process";
import {
  cpSync,
  existsSync,
  lstatSync,
  mkdirSync,
  readdirSync,
  renameSync,
  rmSync,
} from "node:fs";
import { homedir } from "node:os";
import { basename, dirname, join, resolve } from "node:path";

function printHelp() {
  console.log(`reset-data — wipe nav's local runtime data for a fresh start

Usage: bun run reset-data [options]

Options:
  --purge        Delete immediately instead of moving to a recoverable trash dir
  --keep-blobs   Leave blobs/ and traces/ in place (reset only the DB + stacks)
  --dry-run      Show what would happen without changing anything
  -y, --yes      Skip the confirmation prompt
  --force        Proceed even if a nav-local-backend process is running
  -h, --help     Show this help

Preserved always: settings.json, settings.json.bak-*, nav.log.
Honors NAV_DB_PATH and NAV_STACKS_PATH.`);
}

function parseArgs(argv) {
  const opts = {
    purge: false,
    keepBlobs: false,
    dryRun: false,
    yes: false,
    force: false,
  };
  for (const arg of argv) {
    switch (arg) {
      case "--purge":
        opts.purge = true;
        break;
      case "--keep-blobs":
        opts.keepBlobs = true;
        break;
      case "--dry-run":
        opts.dryRun = true;
        break;
      case "-y":
      case "--yes":
        opts.yes = true;
        break;
      case "--force":
        opts.force = true;
        break;
      case "-h":
      case "--help":
        printHelp();
        process.exit(0);
        break;
      default:
        console.error(`unknown option: ${arg}`);
        printHelp();
        process.exit(2);
    }
  }
  return opts;
}

/** Total size in bytes of a file or (recursively) a directory. */
function pathSize(target) {
  // lstat (not stat) so a dangling symlink can't throw and a symlink's
  // target size isn't counted toward the data we're about to remove.
  const st = lstatSync(target);
  if (st.isSymbolicLink() || !st.isDirectory()) {
    return st.size;
  }
  let total = 0;
  for (const entry of readdirSync(target)) {
    total += pathSize(join(target, entry));
  }
  return total;
}

function formatBytes(bytes) {
  const units = ["B", "KB", "MB", "GB"];
  let size = bytes;
  let unit = 0;
  while (size >= 1024 && unit < units.length - 1) {
    size /= 1024;
    unit += 1;
  }
  return `${size.toFixed(unit === 0 ? 0 : 1)} ${units[unit]}`;
}

/** PIDs of any running nav-local-backend, so we never wipe an open DB. */
function findRunningBackend() {
  const res = spawnSync("pgrep", ["-f", "nav-local-backend"], {
    encoding: "utf8",
  });
  // Couldn't run the check at all (e.g. pgrep not on PATH): return null so the
  // caller warns instead of silently assuming nothing is running.
  if (res.error || typeof res.status !== "number") {
    return null;
  }
  // pgrep exit codes: 0 = matches, 1 = no matches, >1 = error.
  if (res.status === 1) {
    return [];
  }
  if (res.status !== 0) {
    return null;
  }
  return (res.stdout ?? "")
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean);
}

function confirm(question) {
  const answer = prompt(question);
  if (answer === null) {
    return false;
  }
  return ["y", "yes"].includes(answer.trim().toLowerCase());
}

/** Move into trash, falling back to copy+remove across filesystems. */
function moveToTrash(target, trashDir) {
  const dest = join(trashDir, basename(target));
  try {
    renameSync(target, dest);
  } catch (err) {
    if (err?.code === "EXDEV") {
      cpSync(target, dest, { recursive: true });
      rmSync(target, { recursive: true, force: true });
    } else {
      throw err;
    }
  }
}

function main() {
  const opts = parseArgs(process.argv.slice(2));
  const home = homedir();
  const navDir = join(home, ".nav");

  const dbPath = resolve(
    process.env.NAV_DB_PATH?.trim() || join(navDir, "nav.db"),
  );
  const stacksPath = resolve(
    process.env.NAV_STACKS_PATH?.trim() || join(navDir, "stacks.jsonl"),
  );
  // Artifacts live beside the database; anchoring to the DB's dir means an
  // overridden NAV_DB_PATH won't wipe the unrelated default ~/.nav/blobs.
  const dataDir = dirname(dbPath);

  // Session DB trio + stacks log, plus artifact dirs unless kept.
  const targets = [dbPath, `${dbPath}-wal`, `${dbPath}-shm`, stacksPath];
  if (!opts.keepBlobs) {
    targets.push(join(dataDir, "blobs"), join(dataDir, "traces"));
  }

  // Defense in depth: never let a target escape into settings/backups/home.
  const present = targets.filter((target) => {
    if (target === dataDir || target === navDir || target === home) {
      throw new Error(`refusing to operate on ${target}`);
    }
    const name = basename(target);
    if (name === "nav.log" || name.startsWith("settings.json")) {
      throw new Error(`refusing to delete protected file ${target}`);
    }
    return existsSync(target);
  });

  if (present.length === 0) {
    console.log("Nothing to reset — nav's data dir is already clean.");
    return;
  }

  console.log(`nav data dir: ${dataDir}`);
  let totalBytes = 0;
  for (const target of present) {
    const size = pathSize(target);
    totalBytes += size;
    const kind = lstatSync(target).isDirectory() ? "dir " : "file";
    console.log(`  ${kind} ${target}  (${formatBytes(size)})`);
  }
  console.log(`total: ${formatBytes(totalBytes)}`);

  const found = findRunningBackend();
  const pids = found ?? [];
  const backendRunning = pids.length > 0;
  if (found === null) {
    console.error(
      "\nCould not check for a running nav-local-backend (pgrep unavailable).",
    );
    console.error("Make sure it is stopped before continuing.");
  } else if (backendRunning) {
    console.error(
      `\nnav-local-backend appears to be running (pid ${pids.join(", ")}).`,
    );
    console.error("Wiping the DB while it is open risks corruption.");
  }

  if (opts.dryRun) {
    const action = opts.purge ? "delete" : "move to trash";
    console.log(`\n[dry-run] would ${action} the above. No changes made.`);
    if (backendRunning && !opts.force) {
      console.log("(a real run would refuse until you stop it, or --force)");
    }
    return;
  }

  if (backendRunning && !opts.force) {
    console.error("Stop it first, or re-run with --force to override.");
    process.exit(1);
  }
  if (backendRunning) {
    console.error("Proceeding anyway because --force was given.");
  }

  if (!opts.yes) {
    const verb = opts.purge ? "permanently delete" : "move to trash";
    if (!confirm(`\nProceed to ${verb} this data? [y/N] `)) {
      console.log("Aborted.");
      return;
    }
  }

  // Process each item independently so one failure can't leave a silent,
  // half-finished wipe — report exactly what did and didn't get handled.
  const failed = [];
  if (opts.purge) {
    for (const target of present) {
      try {
        rmSync(target, { recursive: true, force: true });
      } catch (err) {
        failed.push(`${target}: ${err.message}`);
      }
    }
    console.log(`\nDeleted ${present.length - failed.length} item(s).`);
  } else {
    const stamp = new Date().toISOString().replace(/[:.]/g, "-");
    const trashDir = join(dataDir, ".reset-trash", `reset-${stamp}`);
    mkdirSync(trashDir, { recursive: true });
    for (const target of present) {
      try {
        moveToTrash(target, trashDir);
      } catch (err) {
        failed.push(`${target}: ${err.message}`);
      }
    }
    console.log(
      `\nMoved ${present.length - failed.length} item(s) to:\n  ${trashDir}`,
    );
    console.log("Restore by moving items back from there.");
    console.log(`Reclaim the space with: rm -rf "${trashDir}"`);
  }

  if (failed.length > 0) {
    console.error(`\n${failed.length} item(s) could not be processed:`);
    for (const item of failed) {
      console.error(`  ${item}`);
    }
    process.exitCode = 1;
  }

  console.log("nav will recreate a fresh database on its next launch.");
}

main();
