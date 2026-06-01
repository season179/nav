# Bash/Shell Tool Comparison Across Local Coding Agents

Date: 2026-06-01

Scope: local repositories under `/Users/season/Personal`, with `nav` from this checkout as the baseline. This report intentionally covers only the bash/shell/exec-style tool surface. No online research was used.

## Executive Summary

`nav` has the smallest shell tool in the set. It is a trusted-local `bash` tool that runs `sh -c` in the session working directory, caps the tail of combined output at 2000 lines or 50 KB, and stops on timeout or cooperative cancellation. It does not implement approvals, sandboxing, process-tree cleanup, background jobs, streaming, or full-output persistence.

The closest implementation lineage is `pi`: same tool name, same 2000 line / 50 KB tail cap, same basic "run a command in cwd" contract. `pi` adds process-tree killing, streaming progress, full-output temp files, shell selection, and extension hooks.

The more production-hardened agents split into three directions:

- Safety-first shells: `codex`, Claude Code, `opencode`, `crush`, `kimiflare`, `hermes-agent`, and `forgecode` add permissions, policy checks, approval prompts, sandboxing, or command classification.
- Long-running command handling: Claude Code, `crush`, `codex`, and `hermes-agent` support background/pollable work; `codex` also supports interactive exec sessions and stdin.
- Output durability: `pi`, `opencode`, Claude Code, `forgecode`, and `kimiflare` preserve or expose full logs when inline output is reduced.

## Included Projects

| Project | Local path | Shell-like tool names | Included because |
| --- | --- | --- | --- |
| nav | `/Users/season/.codex/worktrees/de51/nav` | `bash` | Baseline implementation in the current checkout. |
| pi | `/Users/season/Personal/pi` | `bash` | Coding-agent package with a direct bash tool. |
| opencode | `/Users/season/Personal/opencode` | `bash`, shell kinds internally | Coding agent with a first-class shell tool. |
| codex | `/Users/season/Personal/codex` | `shell_command`, `exec_command` | Coding agent with classic and unified shell execution paths. |
| crush | `/Users/season/Personal/crush` | `bash`, `job_output`, `job_kill` | Terminal coding assistant with foreground/background shell tools. |
| Claude Code 2.1.88 | `/Users/season/Personal/claude-code-2.1.88` | `Bash` | Local extracted implementation of Claude Code's bash tool. |
| hermes-agent | `/Users/season/Personal/hermes-agent` | `terminal`, `process` | Agent framework with a terminal tool and managed background processes. |
| kimiflare | `/Users/season/Personal/kimiflare` | `bash` | Terminal coding agent with configurable shell and permission flow. |
| forgecode | `/Users/season/Personal/forgecode` | `shell` | Coding agent with a shell service and policy layer. |

## Excluded Or Not Directly Comparable

| Project | Reason |
| --- | --- |
| `/Users/season/Personal/nanoclaw` | Runs agents inside containers and relies on the underlying provider/SDK for shell access. It has container isolation and host-side command gates, but no independent bash tool implementation comparable to `nav`. |
| `/Users/season/Personal/flue` | Provides a Cloudflare Worker code tool. Its Workspace sandbox explicitly does not support `exec()`, so it is intentionally not a bash/shell tool. |
| `/Users/season/Personal/t3code` | Acts as an agent/desktop/orchestration stack. The search surfaced protocol and SSH utilities, not a first-class model-facing bash tool. |
| `/Users/season/Personal/vibe-kanban` | Orchestrates external coding agents and their permission settings. It does not own the bash tool behavior. |
| `/Users/season/Personal/pi-squire` | Delegates to Pi through `acpx`; it documents permission posture but does not implement its own shell tool. |
| `/Users/season/Personal/smartsidekick` and utility repos | No comparable model-facing bash/shell tool found in the local scan. |

## Side-By-Side Contract

| Agent | Model-facing args | Default timeout | Working directory | Shell/backend | Background/interactive |
| --- | --- | --- | --- | --- | --- |
| nav | `command`, optional `timeout` seconds | 120s | Session cwd passed by registry | `sh -c` | None |
| pi | `command`, optional `timeout` seconds | No default timeout in tool schema | Session cwd, with optional command prefix/hooks | Configurable shell, usually `/bin/bash -c`, fallback `sh`; Git Bash on Windows | None |
| opencode | `command`, required `description`, optional `timeout` ms, optional `workdir` | 120s by default flag | `workdir` or session cwd | Shell abstraction: zsh/bash/sh on Unix, cmd/PowerShell/Git Bash on Windows | No tool-level background; separate shell mode/API exists |
| codex | `shell_command`: `command`, `workdir`, `timeout_ms`; `exec_command`: `cmd`, `workdir`, `shell`, `tty`, `yield_time_ms`, `max_output_tokens`, etc. | Classic shell path defaults to a short 10s timeout unless specified | Explicit or session cwd | User shell via runtime; unified exec can run with PTY | Yes: unified exec can return `session_id`, accept stdin, and poll output |
| crush | `description`, `command`, `working_dir`, `run_in_background`, `auto_background_after` | Auto-background after 60s by default | Explicit `working_dir` | `mvdan/sh` POSIX interpreter, not the user's real shell | Yes: background shell manager plus `job_output` and `job_kill` |
| Claude Code | `command`, optional `timeout` ms, optional `description`, `run_in_background`, `dangerouslyDisableSandbox` | 120s, max 600s by default | Persistent app cwd; commands can change cwd unless prevented for subagents | User bash/zsh provider, sandbox wrapper when enabled | Yes: explicit background, Ctrl+B foreground backgrounding, auto-background in assistant mode |
| hermes-agent | `command`, `background`, `timeout`, `task_id`, `force`, `workdir`, `pty`, `notify_on_complete`, `watch_patterns` | 180s config default; foreground max 600s by default | Session cwd, optional `workdir`, persistent cwd marker | Local bash plus Docker, Singularity, Modal, Daytona, SSH backends | Yes: process registry, PTY option, stdin management via `process` tool |
| kimiflare | `command`, optional `timeout_ms` | 120s, max 600s | Tool context cwd | `bash -lc` on Unix, cmd.exe on Windows, PowerShell or custom path via `/shell` | None |
| forgecode | `command`, optional `cwd`, `keep_ansi`, env var names, optional `description` | Tool-registry timeout from `FORGE_TOOL_TIMEOUT`, default 300s | Explicit `cwd` preferred; prompt forbids `cd` | Configured `env.shell`, `-c` or `/C` | None |

## Execution And Process Control

| Agent | Process model | Cancellation/timeout behavior | Process-tree handling |
| --- | --- | --- | --- |
| nav | One child process per call. Drains stdout and stderr on two threads. | Polls every 50ms; `child.kill()` on timeout/cancel. | Kills only the spawned shell process. Grandchildren may survive if detached or reparented. |
| pi | One child process per call with streaming accumulator. | Optional timeout and AbortSignal. | Kills the process tree, including detached process tracking on Unix. |
| opencode | Detached child on non-Windows with Effect runtime. Streams combined output. | Abort/timeout kills process and forces after grace period. | Stronger than `nav`; kill path is part of runtime process abstraction. |
| codex | Tool orchestration wraps sandbox/runtime execution. Unified exec keeps managed process sessions. | Cancellation token, timeout, and permission/network denial cancellation. | Runtime-level cleanup and process manager; unified exec caps concurrent processes. |
| crush | Foreground command starts through a background manager, then waits or auto-backgrounds. | Auto-background threshold rather than a hard foreground timeout. | Background manager owns jobs and exposes kill. |
| Claude Code | Each command spawns a shell process; output goes to a task output file or pipe. | Timeout kills unless auto-backgrounding is enabled. Abort kills except special interrupt/background paths. | Uses `tree-kill` for the child process tree. |
| hermes-agent | Local backend spawns bash with `setsid`; non-local backends execute inside environment adapters. | Poll loop checks interrupt and deadline; timeout returns code 124. | Local backend kills the process group with SIGTERM then SIGKILL. |
| kimiflare | Node `spawn` per call. | Timer sends SIGKILL on timeout; AbortSignal sends SIGKILL. | Kills the spawned shell only. It destroys stdout/stderr streams on shell exit to avoid pipe hangs from backgrounded grandchildren. |
| forgecode | Tokio child process per call, guarded by a mutex so shell commands run one at a time. | Tool registry wraps calls in a global tool timeout. | Uses `kill_on_drop`; no dedicated process-tree kill found. |

## Output Handling

| Agent | Output shape | Truncation / persistence |
| --- | --- | --- |
| nav | Concatenates stdout then stderr; appends status notes like `[exited with status 3]`. Stderr is not chronologically interleaved. | Keeps tail only: last 2000 lines or 50 KB. No full-output file. |
| pi | Streams partial output and combines stdout/stderr into the accumulator. Nonzero exits throw with output included. | Same 2000 line / 50 KB tail cap as `nav`, plus full output saved to a temp file if truncated. |
| opencode | Streams combined stdout/stderr into metadata and returns a tail preview. | Defaults to 2000 lines / 50 KB. Full output is saved to a truncation file when bytes exceed the cap. |
| codex | Classic path formats exit code, wall time, and output. Unified exec uses a head/tail buffer and token budget. | Output caps vary by path; unified exec retains a 1 MiB head/tail buffer and returns model-sized slices. |
| crush | Returns structured output with cwd, stdout, stderr, exit code/error. | Truncates stdout and stderr separately to 30,000 chars using first/last halves. |
| Claude Code | Merges stdout/stderr into one file-backed stream for chronological output, then maps to text/image/tool-result blocks. | Inline cap is 30,000 chars. Large output is linked/copied into tool-results for later reading, capped at 64 MB persisted. |
| hermes-agent | Returns JSON with `output`, `exit_code`, `error`, and optional approval/meaning fields. Strips ANSI and redacts secrets. | Uses configured max bytes; if too long, keeps roughly 40 percent head and 60 percent tail. Background logs are readable through `process`. |
| kimiflare | Header plus `--- stdout ---` and `--- stderr ---`, with exit/timeout/abort marker. | Reducer compacts large outputs and stores artifacts for `expand_artifact`. Diff-style git output bypasses reduction but is still archived. |
| forgecode | XML-like `<shell_output>` with command, shell, optional exit code, stdout/stderr sections. | Prefix/suffix line truncation plus per-line max length. Full stdout/stderr can be written to temp files and read separately. |

## Safety And Permissions

| Agent | Permission/sandbox posture |
| --- | --- |
| nav | Trusted local. Path tools refuse workspace escape, but `bash` is explicitly exempt and runs with backend user privileges. No approval prompt, no sandbox, no command policy. |
| pi | No approval or sandbox inside the bash tool itself. Safety is mostly through which tools are exposed and extension-level customization. |
| opencode | Parses shell with tree-sitter to request command-pattern approval and external-directory approval. Supports shell env hooks. |
| codex | Strongest runtime boundary in this set: approval policy, per-turn granted permissions, sandbox selection, network approval, escalation justifications, prefix rules, and runtime enforcement. |
| crush | Permission service asks for execution approval unless the command is classified as safe/read-only. Also has a command blocklist and package-manager restrictions. |
| Claude Code | Permission matcher parses bash commands; read-only classification influences concurrency/permission behavior. Sandboxing can be enabled, with a model-visible `dangerouslyDisableSandbox` escape hatch. |
| hermes-agent | Consolidated approval guard for dangerous commands, gateway pending approvals, sudo prompt/cache handling, workdir injection validation, and env-secret scrubbing. Can run in isolated Docker/Modal/etc. backends. |
| kimiflare | `needsPermission: true`; plan mode allows read-only bash but blocks mutating bash. Edit mode asks externally; auto mode allows. PreToolUse hooks can veto. |
| forgecode | Policy service checks operations. Default bundled policy allows all commands, but the architecture supports command policies. Prompt describes restricted shell mode and recommends specialized file tools. |

## Detailed Notes

### nav

Sources:

- `/Users/season/.codex/worktrees/de51/nav/src/tools/builtins/bash.rs`
- `/Users/season/.codex/worktrees/de51/nav/src/tools/mod.rs`
- `/Users/season/.codex/worktrees/de51/nav/src/tools/support/truncate.rs`
- `/Users/season/.codex/worktrees/de51/nav/src/tools/tests.rs`

`nav` is intentionally simple. The bash tool accepts only a command and optional timeout in seconds. It spawns `sh -c <command>` in the registry-provided cwd, drains stdout and stderr on separate threads, waits in a 50ms polling loop, and kills the child on timeout or cancellation.

The output cap is centralized and shared with other tools: 2000 lines or 50 KB. For command output, `cap_tail` keeps the most recent output. Because stdout and stderr are drained separately and then concatenated, `nav` loses chronological interleaving.

The module-level safety comment is explicit: tools run with backend user privileges, path tools refuse workspace escape, and `bash` is exempt from workspace confinement. That makes `nav` the simplest trusted-local shell in the comparison.

### pi

Sources:

- `/Users/season/Personal/pi/packages/coding-agent/src/core/tools/bash.ts`
- `/Users/season/Personal/pi/packages/coding-agent/src/core/bash-executor.ts`
- `/Users/season/Personal/pi/packages/coding-agent/src/core/tools/truncate.ts`

`pi` is the most direct reference point for `nav`. The user-facing description mirrors `nav`: execute bash in cwd, cap output to the last 2000 lines or 50 KB, and support an optional timeout.

Key differences from `nav`:

- The timeout is optional with no default timeout at the tool schema level.
- Shell selection is configurable and cross-platform: custom shell path, Git Bash on Windows, `/bin/bash`, PATH bash, then `sh`.
- It streams partial output through an accumulator.
- If output is truncated, the full output is saved to a temp file.
- Timeout and abort paths kill the process tree rather than only the immediate shell.
- `BashOperations` lets extensions delegate execution to another backend, add command prefixes, override shell path, or hook spawn options.

### opencode

Sources:

- `/Users/season/Personal/opencode/packages/opencode/src/tool/shell.ts`
- `/Users/season/Personal/opencode/packages/opencode/src/tool/shell/id.ts`
- `/Users/season/Personal/opencode/packages/opencode/src/tool/shell/prompt.ts`
- `/Users/season/Personal/opencode/packages/opencode/src/shell/shell.ts`
- `/Users/season/Personal/opencode/packages/opencode/src/tool/truncate.ts`

`opencode` keeps the model-facing tool id as `bash` for compatibility, but internally supports a richer shell abstraction: bash, zsh, sh, cmd, PowerShell, and Git Bash depending on platform and configuration.

It is notably more safety-aware than `nav`. It parses commands with tree-sitter to identify permission patterns and external directories, then asks for approval. It also has a `shell.env` plugin hook for environment shaping.

Output behavior is close to `nav` in default caps, but more durable: it keeps the tail within 2000 lines / 50 KB and saves full output to a file when the byte cap is exceeded.

### codex

Sources:

- `/Users/season/Personal/codex/codex-rs/core/src/tools/handlers/shell.rs`
- `/Users/season/Personal/codex/codex-rs/core/src/tools/handlers/shell/shell_command.rs`
- `/Users/season/Personal/codex/codex-rs/core/src/tools/handlers/unified_exec/exec_command.rs`
- `/Users/season/Personal/codex/codex-rs/core/src/unified_exec/process_manager.rs`
- `/Users/season/Personal/codex/codex-rs/core/src/unified_exec/process.rs`
- `/Users/season/Personal/codex/codex-rs/core/src/tools/runtimes/shell.rs`

Codex has two shell-like surfaces. The older `shell_command` accepts command, workdir, timeout, login flag, and approval parameters. The newer `exec_command` accepts a richer command/session shape: shell override, TTY, yield timing, max output tokens, environment id, and approval parameters.

Compared to `nav`, Codex is a full runtime and policy system. Shell execution flows through approval checks, sandbox selection, per-turn permission grants, optional escalation, network policy review, and runtime cancellation. The unified exec path can return a `session_id`, then `write_stdin` can continue, poll, or feed a running command.

The tradeoff is complexity. Codex's shell tool is not just a function that spawns a process; it is a managed execution subsystem.

### crush

Sources:

- `/Users/season/Personal/crush/internal/agent/tools/bash.go`
- `/Users/season/Personal/crush/internal/agent/tools/bash.md.tpl`
- `/Users/season/Personal/crush/internal/shell/run.go`
- `/Users/season/Personal/crush/internal/shell/background.go`
- `/Users/season/Personal/crush/internal/agent/tools/safe.go`
- `/Users/season/Personal/crush/internal/agent/tools/job_output.go`
- `/Users/season/Personal/crush/internal/agent/tools/job_kill.go`

`crush` exposes a `bash` tool but runs commands through the Go `mvdan/sh` POSIX interpreter. This is portable and controllable, but it is not the user's real shell.

Its model contract is more explicit than `nav`: commands have descriptions, working directories, optional explicit backgrounding, and an `auto_background_after` threshold. Safe read-only commands can bypass approval, while other commands go through the permission service. It also blocks high-risk command families and global package-manager installs.

For long-running work, `crush` has a first-class background manager with `job_output` and `job_kill`. This is the clearest contrast with `nav`, which has only foreground command execution.

### Claude Code 2.1.88

Sources:

- `/Users/season/Personal/claude-code-2.1.88/source/src/tools/BashTool/BashTool.tsx`
- `/Users/season/Personal/claude-code-2.1.88/source/src/tools/BashTool/bashPermissions.ts`
- `/Users/season/Personal/claude-code-2.1.88/source/src/tools/BashTool/prompt.ts`
- `/Users/season/Personal/claude-code-2.1.88/source/src/utils/Shell.ts`
- `/Users/season/Personal/claude-code-2.1.88/source/src/utils/ShellCommand.ts`
- `/Users/season/Personal/claude-code-2.1.88/source/src/tasks/LocalShellTask/LocalShellTask.tsx`

Claude Code's `Bash` tool is one of the most feature-complete local shell tools in the set. It defaults to a 2 minute timeout and a 10 minute max, both configurable by env vars. It can run in the background, auto-background long-running assistant-mode commands, surface progress after 2 seconds, and notify the model when a background task completes.

It uses a bash/zsh provider and can wrap execution in a sandbox. Permission matching is based on parsed commands, and there is a `dangerouslyDisableSandbox` parameter for explicit sandbox override.

The implementation puts stdout and stderr into a shared file descriptor for chronological output, polls that file for progress, persists large outputs into a tool-results directory, and uses `tree-kill` for process-tree cleanup.

### hermes-agent

Sources:

- `/Users/season/Personal/hermes-agent/tools/terminal_tool.py`
- `/Users/season/Personal/hermes-agent/tools/environments/local.py`
- `/Users/season/Personal/hermes-agent/tools/environments/base.py`
- `/Users/season/Personal/hermes-agent/tools/process_registry.py`
- `/Users/season/Personal/hermes-agent/tools/approval.py`

Hermes exposes `terminal` rather than `bash`, but it is clearly a shell execution tool. It supports local, Docker, Singularity, Modal, Daytona, and SSH backends. The local backend is spawn-per-call, uses bash, preserves cwd across calls via a marker file, and uses a sanitized subprocess environment that strips Hermes-managed secrets.

Foreground commands default to the configured `TERMINAL_TIMEOUT` (180s by default) and are capped by `TERMINAL_MAX_FOREGROUND_TIMEOUT` (600s by default). Background commands are first-class: they return a session id, can use PTY mode locally, can notify on completion, and can watch for rare output patterns.

Hermes has the strongest command-lifecycle cleanup outside Codex/Claude. Local commands run in their own process group; timeout, interrupt, or exception paths terminate the group with SIGTERM then SIGKILL. It also validates `workdir` to avoid shell metacharacter injection and routes dangerous commands through approval guards.

### kimiflare

Sources:

- `/Users/season/Personal/kimiflare/src/tools/bash.ts`
- `/Users/season/Personal/kimiflare/src/tools/executor.ts`
- `/Users/season/Personal/kimiflare/src/sdk/permissions.ts`
- `/Users/season/Personal/kimiflare/src/config.ts`

Kimiflare's `bash` tool is compact like `nav`, but with permission and output-reduction layers around it. It accepts `command` and optional `timeout_ms`, defaults to 120s, caps at 600s, and uses configurable shell selection. `/shell` can set `auto`, `bash`, `cmd`, `powershell`, or a custom shell path.

The tool itself kills only the spawned shell on timeout/abort. It adds a practical pipe-hang fix: when the shell exits, it destroys stdout/stderr streams so backgrounded grandchildren that inherited pipes do not keep the Promise open forever.

The executor provides the stronger safety/output story: every bash call needs permission, plan mode allows read-only bash, hooks can veto calls, and large outputs are reduced while preserving artifacts for `expand_artifact`.

### forgecode

Sources:

- `/Users/season/Personal/forgecode/crates/forge_domain/src/tools/catalog.rs`
- `/Users/season/Personal/forgecode/crates/forge_domain/src/tools/descriptions/shell.md`
- `/Users/season/Personal/forgecode/crates/forge_services/src/tool_services/shell.rs`
- `/Users/season/Personal/forgecode/crates/forge_infra/src/executor.rs`
- `/Users/season/Personal/forgecode/crates/forge_app/src/tool_registry.rs`
- `/Users/season/Personal/forgecode/crates/forge_app/src/truncation/truncate_shell.rs`

Forge exposes `shell` with `command`, optional `cwd`, `keep_ansi`, env var names to pass through, and an optional description. Its prompt strongly discourages using shell for file read/search/edit operations and forbids `cd` in favor of the `cwd` parameter.

Execution runs through a configured shell from the environment (`SHELL` on Unix, `COMSPEC` on Windows), streams stdout/stderr to the console while capturing both, and serializes shell commands with a mutex. The tool registry applies a configurable global tool timeout, defaulting to 300 seconds.

Forge has a policy abstraction, but the bundled default policy allows all commands. Its output formatting is stronger than `nav`: stdout and stderr are separately tagged, prefix/suffix truncated by line count, long lines are clipped, and full streams can be written to temp files for later reads.

## What This Suggests For nav

### Keep As-Is If The Product Goal Is Trusted Local Simplicity

`nav` is easy to audit. Its bash tool is small enough to reason about in one screen, and the model contract is straightforward. That simplicity is real value.

### Low-Cost Improvements

These would preserve the small trusted-local design:

1. Kill process groups rather than only the shell child on timeout/cancel.
2. Use a configured/user shell or `/bin/bash` if the tool is called `bash`; currently it runs `sh -c`.
3. Preserve full command output to a temp file when truncation happens, matching `pi` and `opencode`.
4. Interleave stdout/stderr chronologically by using a shared pipe/file or async stream reader.
5. Return more structured metadata internally: exit code, timed out, cancelled, truncated, full-output path.

### Medium-Scope Improvements

These change product behavior but are still compatible with a local app:

1. Add optional streaming/progress updates for long commands.
2. Add a managed background command mode with `session_id`, `poll`, and `kill`.
3. Add a configurable shell path and environment shaping hook.
4. Add a minimal permission hook so Electron/UI can ask before high-risk command families.

### Larger Directional Choices

If `nav` is meant to become a safer multi-project or semi-remote agent, borrow from `codex`, `opencode`, and Claude Code:

1. Parse command structure for approval patterns rather than matching raw strings only.
2. Add workspace/external-directory permission checks.
3. Add sandbox profiles or explicit permission profiles.
4. Treat network access as a separate approval dimension.

The most sympathetic near-term path is probably `pi` plus one piece from Hermes: keep the simple tool contract, but add full-output persistence and process-group cleanup. That gets most of the practical reliability win without turning `nav`'s bash tool into a runtime subsystem.
