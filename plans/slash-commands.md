# Slash commands in nav

Status: planning. No implementation in this document.

Related: [architecture.md](./architecture.md), [pi-tools-in-nav.md](./pi-tools-in-nav.md), [session-storage.md](./session-storage.md), [session-storage-sequence.md](./session-storage-sequence.md).

Research sources:

- [Pi slash commands](../research/slash-commands/pi-slash-commands.md)
- [Codex slash commands](../research/slash-commands/codex-slash-commands.md)
- [Claude Code 2.1.88 slash commands](../research/slash-commands/claude-code-2.1.88-slash-commands.md)
- Current nav seams: `tui/src/commands/slash.ts`, `tui/src/app/App.tsx`, `tui/src/regions/composer/ComposerRegion.tsx`, `tui/src/backend/client.ts`, `crates/nav-protocol/src/rpc.rs`

---

## Summary

Use Pi's design as nav's spine: split slash commands into **interactive frontend commands** and **model-facing prompt commands**.

Do not copy Pi wholesale. Nav should keep Pi's clean TUI/backend separation, but borrow the stronger parts of Codex and Claude Code:

1. A typed command registry with metadata, not ad hoc parser branches.
2. Explicit command effects: local UI, backend session operation, or prompt expansion.
3. Unknown command-looking input should preserve the draft and show an error.
4. Path-like slash input such as `/Users/season/file.txt` should still be allowed as ordinary prompt text.
5. Queued slash input should become a structured queued action once nav supports mid-run queueing.

Skills are explicitly out of scope for slash commands. Nav should follow the Codex-style `$` skill surface for skills instead of adding `/skill:name` or skill aliases to the slash registry. That needs its own plan.

Current nav is much smaller than all three references: the TUI recognizes only `/model`, `/exit`, and `/quit`, and dispatch happens directly in `App.tsx` before normal `session.sendMessage`. That is a good starting point, but the parser should become a registry before adding more commands.

---

## Target model

Slash commands have one shared descriptor shape and one of three execution kinds.

| Kind | Owner | Model query? | Examples |
| --- | --- | --- | --- |
| `local-ui` | TUI | No | `/model`, `/exit`, `/quit`, `/clear`, `/help` |
| `session-rpc` | Backend protocol | No by default | `/new`, `/compact` |
| `prompt` | Backend harness | Yes, after expansion | Markdown prompt templates, future workflow commands |

Design rule: local UI commands never enter `session.sendMessage`. Prompt commands are reusable because they expand inside the backend prompt path. Session commands are backend operations invoked through the generic `commands.run` RPC, not hidden model prompts.

This preserves nav's architecture boundary:

```text
TUI local registry
  handles local-ui commands
  asks backend for command metadata
  dispatches session-rpc commands
  submits ordinary/prompt text

nav-server / nav-protocol
  exposes command discovery and session operation RPCs
  stays frontend-agnostic

nav-harness
  discovers prompt commands
  expands prompt commands before appending the user turn
  owns command source/provenance metadata
```

---

## Command descriptor

Use a boring typed descriptor, shared conceptually across TUI and backend responses.

Suggested fields:

| Field | Purpose |
| --- | --- |
| `name` | Canonical slash name without `/` |
| `aliases` | Alternate names such as `quit` for `exit` |
| `description` | Popup/help text |
| `argumentHint` | Optional inline hint such as `<session>` |
| `kind` | `local-ui`, `session-rpc`, or `prompt` |
| `source` | `builtin`, `project`, `user`, `plugin`, etc. |
| `sourceLabel` | Human-readable source for popup/help |
| `hidden` | Hide from default popup but still allow exact lookup |
| `supportsArgs` | Whether `/name args` dispatches as command |
| `availableWhileBusy` | Whether it can run during an active turn |
| `remoteSafe` | Whether future remote/bridge clients may invoke it |

For `session-rpc`, descriptors are invoked by canonical `name` through `commands.run`. Keep explicit RPC methods for protocol primitives such as `session.sendMessage`, `tool.approve`, and `tool.reject`; use `commands.run` for backend slash-command actions.

---

## Parsing and validation

Keep parsing deliberately small.

Rules:

1. Slash command parsing only activates when the submitted text starts with `/` after trimming.
2. A leading space should escape command parsing and submit literal text.
3. The command token is the first non-whitespace token after `/`.
4. Command-looking names use `[a-zA-Z0-9:_-]`.
5. Tokens containing another `/`, or actual absolute paths, should be treated as ordinary prompt text.
6. Bare commands and inline commands are separate decisions: `/clear` is bare, `/review focus` is inline only if the descriptor supports args.

Unknown behavior:

- Unknown command-looking input: show `Unknown command: /name`, preserve the draft, do not send to the model.
- Unknown path-like slash input: submit as ordinary text.
- Unknown prompt command in backend non-interactive use: return a structured error for command-looking input, pass path-like text through.

This is stricter than Pi and less surprising for a coding TUI.

---

## Autocomplete and help

The composer popup should be a view over command descriptors, not the executor.

Activation:

- Only on the first line.
- Only while the cursor is in the initial `/name` token.
- Do not compete with future file or mention completion once the cursor is inside an argument token.

First version:

- Merge local TUI descriptors with backend `commands.list` descriptors.
- Prefix or fuzzy filter by command name and aliases.
- Display name, description, argument hint, and source label.
- Enter dispatches the selected command.
- Tab completes the command text.
- Esc dismisses the popup.

Later:

- Argument completions for prompt command arguments.
- Recent command grouping.
- Hidden aliases that appear only when typed.
- A `/help` view generated from the same registry.

---

## Backend protocol

Add command discovery before backend command execution.

### `commands.list`

Returns backend-owned command descriptors for the active cwd/session.

Suggested params:

```json
{
  "sessionId": "optional",
  "cwd": "optional",
  "clientKind": "tui"
}
```

Suggested result:

```json
{
  "commands": [
    {
      "name": "review",
      "aliases": [],
      "description": "Review the current diff",
      "argumentHint": "[focus]",
      "kind": "prompt",
      "source": "project",
      "sourceLabel": "project command",
      "supportsArgs": true,
      "availableWhileBusy": false,
      "remoteSafe": true
    }
  ]
}
```

Local UI built-ins should not come from `commands.list`; the TUI owns them. The TUI merges backend descriptors with its local registry for autocomplete and help.

### `commands.run`

Invoke backend-owned `session-rpc` commands by canonical name.

Suggested params:

```json
{
  "sessionId": "optional",
  "name": "compact",
  "arguments": ""
}
```

Suggested result:

```json
{
  "name": "compact",
  "status": "accepted",
  "message": "Compaction started"
}
```

The first built-in session commands should be:

- `new`
- `compact`

`compact` should follow Codex closely: `/compact` is a built-in session command that starts a dedicated compaction turn. It is not a prompt template, even though the compaction task may synthesize an internal model-facing summary prompt. The command should return quickly after accepting the operation, mark the UI as running, and let backend events report progress/completion.

Codex behavior to mirror:

- `/compact` is unavailable while a normal task is running; if typed during an active turn and queued, it dispatches after that turn completes.
- Once a compact turn is running, the turn is non-steerable; follow-up user messages should queue instead of steering the compact turn.
- The compact task records manual/user-requested compaction metadata and emits a context-compacted event when the replacement history is installed.
- Token usage/status UI should reset or refresh when manual compaction starts.

Defer lower-usage session operations such as `session.resume`, `session.fork`, `settings.reload`, and session metadata views until the core command surface is proven. Session storage work should decide the exact semantics for resume, fork, and lineage before those commands return to scope.

### Prompt expansion

`session.sendMessage` should follow Pi's execution order for model-facing input. It should expand only `prompt` commands. It should not run TUI commands or session management commands by accident.

Expansion order:

1. Future extension commands get first chance to handle `/name` before any prompt expansion.
2. Future input hooks may handle or transform the raw input before expansion.
3. Future `$` skill/context injection runs in the same conceptual slot Pi gives `/skill:name`, but skills remain out of slash command scope.
4. Markdown prompt command expansion runs for `.nav/commands/*.md`.
5. Ordinary prompt text continues if the input is not command-looking.
6. Structured unknown-command error returns if command-looking input is unresolved.

For v1, extension commands, input hooks, and skills are not in this slash-command implementation. The effective v1 path is therefore `.nav/commands` prompt expansion before provider submission, with the earlier Pi slots reserved so the runtime shape does not need to change later.

Representation decision:

- Render and preserve the original invocation as the visible user turn, for example `/review storage conflicts`.
- Send the expanded prompt text to the provider.
- Persist the expanded prompt snapshot in turn metadata with command name, args, source path, source hash/version, and expansion timestamp.
- Replay of an existing session should use the stored expanded snapshot. Re-running the command should expand from the current command file.

---

## Prompt command sources

Start with Markdown prompt templates. Defer executable extension commands. Keep skills out of this plan; `$` skill invocation should be designed separately.

### Markdown commands

Project path:

```text
.nav/commands/*.md
```

Discovery:

- Filename becomes command name.
- Optional frontmatter fields: `description`, `argument-hint`, `allowed-tools`, `model`, `effort`, `user-invocable`.
- First non-empty body line can be the fallback description.
- Only scan nav-owned command directories. Do not load `.claude/commands`, Pi command folders, or other ecosystems' command directories for compatibility.
- Project discovery should stop at the git root or home boundary so commands do not leak across unrelated repos.

Expansion:

- Parse shell-ish quoted args.
- Support `$ARGUMENTS`, `$1`, `$2`, `$@`, and `${@:2}`.
- If no placeholder exists, append an `ARGUMENTS:` block.
- Expanded content becomes model-facing prompt text with source metadata.

Do not support shell interpolation in v1. It widens the trust boundary and duplicates tool approval concerns.

---

## Issue carve-up

Use `SC-*` for slash-command issues.

| # | Issue | Model | Scope |
| --- | --- | --- | --- |
| SC-01 | Typed local slash registry | weak | Replace `parseSlashCommand` branches with local descriptors for `/model`, `/exit`, `/quit`; add `/clear` and `/help` if small. Preserve current behavior. Add parser tests for aliases, args, path-like input, and leading-space escape. |
| SC-02 | Unknown command UX and draft preservation | weak | Unknown command-looking input shows a system error and preserves the composer draft. Path-like slash input still sends as text. Requires a small App/composer state adjustment because current submit clears input before dispatch. |
| SC-03 | Composer slash popup | strong | First-line command popup over local descriptors. Keyboard support: up/down, enter, tab, esc. Snapshot tests for narrow widths and no overlap with composer hint. No backend dependency. |
| SC-04 | Backend command descriptor protocol | strong | Add protocol structs, `commands.list`, `commands.run`, server routing, and TUI client methods. Backend may return an empty list at first. TUI merges local + backend descriptors for popup/help. |
| SC-05 | Markdown prompt commands | strong | Add harness command discovery for `.nav/commands/*.md`, frontmatter parsing, arg substitution, source metadata, and expansion in `session.sendMessage`. Include backend tests proving expanded text reaches the provider request and unknown command-looking input does not. |
| SC-06 | Essential session slash commands | strong | Add descriptors and `commands.run` handlers for `/new` and `/compact` as session-rpc commands. `/compact` starts a Codex-style standalone compaction turn, not a prompt template. Defer `/resume`, `/fork`, `/reload`, and `/session` because they are low-frequency workflows and depend on more settled storage/session semantics. |
| SC-07 | Structured queue semantics | strong | Once nav supports input while busy, queued slash input records its action: plain prompt, parse slash on dequeue, or immediate local-ui. Prevent a queued `/compact` from becoming literal model text by accident. |
| SC-08 | Remote/headless safety | weak | Add `remoteSafe`, `supportsNonInteractive`, and filtering rules for future non-TUI clients. Prompt commands are generally safe to relay; local UI commands are not; session-rpc commands opt in one by one. |
| SC-09 | Docs and help polish | weak | Generate `/help` from descriptors, document command dirs and conflict rules, and add a short docs page once the behavior is stable. |

---

## Sequencing

```text
SC-01 -> SC-02 -> SC-03
                    |
                    v
                 SC-04
                    |
        +-----------+-----------+
        v                       v
      SC-05                  SC-06

SC-07 waits until nav supports busy-turn input queueing.
SC-08 can land after SC-04 but matters most before non-TUI clients use commands.
SC-09 should wait until SC-05/SC-06 settle names and conflict rules.
```

Recommended first milestone: SC-01 through SC-03. That gives nav a good local slash UX without backend churn.

Recommended second milestone: SC-04 and SC-05. That proves the Pi-style split by making prompt commands backend-owned and reusable outside the TUI.

---

## Command list by milestone

### Milestone 1: local TUI commands

| Command | Kind | Notes |
| --- | --- | --- |
| `/model` | `local-ui` | Existing model picker. |
| `/exit`, `/quit` | `local-ui` | Existing exit aliases. |
| `/clear` | `local-ui` | Clears visible history, not durable session history. |
| `/help` | `local-ui` | Renders descriptors from the local registry; backend descriptors join after SC-04. |

### Milestone 2: prompt commands

| Command | Kind | Notes |
| --- | --- | --- |
| `/<template>` | `prompt` | Markdown files under `.nav/commands/*.md`. |

### Milestone 3: essential session commands

| Command | Kind | Notes |
| --- | --- | --- |
| `/new` | `session-rpc` | Starts a fresh session. |
| `/compact` | `session-rpc` | Starts a standalone compaction turn through `commands.run`; any model-facing compaction prompt is synthesized inside the backend task. |

### Deferred session commands

| Command | Reason to defer |
| --- | --- |
| `/resume [query]` | Low-frequency workflow; needs session listing/search UX and settled resume semantics. |
| `/fork [message]` | Low-frequency workflow; depends on fork semantics from session storage. |
| `/reload` | Low-frequency workflow; can stay outside slash UX until settings reload proves useful interactively. |
| `/session` | Low-frequency workflow; current session metadata can wait until there is a stronger user need. |

---

## Conflict rules

1. Local TUI commands win in the TUI.
2. Backend `commands.list` should still report conflicts with source metadata so the TUI can show diagnostics.
3. Duplicate prompt command names should use Pi-style first-wins resolution with a collision diagnostic naming the winner and loser.
4. Hidden aliases should dispatch on exact match but stay out of the default popup.

Recommended priority:

```text
local-ui builtins
session-rpc builtins
future extension commands
project configured prompt commands
project auto-discovered prompt commands
user configured prompt commands
user auto-discovered prompt commands
future package/plugin prompt commands
```

This matches Pi's source priority shape while keeping nav's tighter slash scope. Pi sorts resource paths before prompt-name dedupe so project resources beat user resources, configured entries beat auto-discovered entries, package resources come last, and the first remaining prompt name wins. Nav should do the same for `.nav/commands` collisions.

Copy Pi's execution order, adapted to nav's scope:

```text
runtime dispatch: extension command -> input hook -> skill expansion -> prompt expansion
RPC discovery: extension commands -> prompt templates -> skills
interactive autocomplete: local built-ins -> prompt templates -> extension commands -> skills
```

For nav v1, skills stay out of slash commands and extension commands are deferred. The practical priority is therefore local/session built-ins first, then `.nav/commands` using the Pi-style project-over-user precedence above. Keep the reserved runtime slots in the backend so future extension/input-hook/skill work can land without reordering prompt commands.

---

## Verification plan

TUI tests:

- Parser handles aliases, inline args, path-like slash input, and leading-space escape.
- Unknown commands preserve the draft.
- Popup opens only in slash context.
- Popup does not resize or overlap the composer at narrow terminal widths.
- `/model` still opens the existing overlay.
- `/clear` and `/help` do not call `session.sendMessage`.

Backend tests:

- `commands.list` returns stable descriptor JSON.
- `commands.run` invokes a known `session-rpc` command by canonical name.
- `commands.run` rejects prompt commands and local UI commands.
- `/compact` starts a standalone compaction turn, not a prompt command expansion.
- Queued `/compact` dispatches after an active normal turn completes.
- User messages submitted while compacting queue instead of steering the compact turn.
- Empty backend command list keeps old clients working.
- Markdown command discovery stops at git root or home boundary.
- Arg substitution handles quoted args and missing placeholders.
- Prompt expansion reaches the provider request before the user turn is stored.
- Prompt command turns render the original slash invocation while preserving the expanded model-facing snapshot in metadata.
- Replay uses the stored expanded snapshot even if the command file changes later.
- Unknown command-looking prompt returns a structured error.
- Path-like slash text passes through.

Protocol/client tests:

- TUI client parses backend descriptors.
- Local and backend descriptors merge without duplicate display rows.
- Conflict metadata is preserved.
- Future queue tests prove queued slash actions are not sent as raw prompt text unless explicitly plain.

---

## Out of scope for v1

- Executable extension commands.
- Shell interpolation inside Markdown commands.
- Plugin command loading.
- MCP command exposure.
- Skills and skill discovery. They should use a separate Codex-style `$` invocation surface, not slash commands.
- Rich argument completion.
- Command history/recall.
- Multi-command batches.
- Remote bridge command execution beyond descriptor filtering.
- Model-invoked command use.
