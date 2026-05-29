# Pi Slash Commands Research

Source project: `/Users/season/Personal/pi` at branch `main`, commit `ce554ad3`, clean worktree. Citations are repo-relative paths under that project.

## Executive Summary

Pi has two slash-command layers:

1. Interactive built-ins are TUI commands such as `/settings`, `/model`, `/compact`, `/resume`, and `/quit`. They are listed for autocomplete in `core/slash-commands.ts`, but their behavior is implemented by `InteractiveMode` before user text reaches `AgentSession.prompt()` (`packages/coding-agent/src/core/slash-commands.ts:18`, `packages/coding-agent/src/core/slash-commands.ts:40`, `packages/coding-agent/src/modes/interactive/interactive-mode.ts:2464`, `packages/coding-agent/src/modes/interactive/interactive-mode.ts:2590`).
2. Core prompt commands are extension commands, skill commands, and prompt templates. These are handled inside `AgentSession.prompt()` and therefore work through interactive mode, RPC `prompt`, SDK-style calls, and initial messages (`packages/coding-agent/src/core/agent-session.ts:962`, `packages/coding-agent/src/core/agent-session.ts:1003`, `packages/coding-agent/src/modes/rpc/rpc-mode.ts:389`, `packages/coding-agent/src/modes/rpc/rpc-mode.ts:410`).

That split is the main design choice. UI commands are not part of the model-facing prompt pipeline. Reusable prompt commands are.

## Input And Autocomplete

Pi's editor only treats slash commands as a menu on the first editor line. Typing `/` at the beginning of the message starts autocomplete, and typing command-name characters keeps it alive while the cursor is in slash-command context (`packages/tui/src/components/editor.ts:1053`, `packages/tui/src/components/editor.ts:1072`, `packages/tui/src/components/editor.ts:2038`, `packages/tui/src/components/editor.ts:2053`).

The autocomplete provider receives one combined list: built-ins, prompt templates, extension commands, and skill commands if `enableSkillCommands` is on for the interactive UI (`packages/coding-agent/src/modes/interactive/interactive-mode.ts:448`, `packages/coding-agent/src/modes/interactive/interactive-mode.ts:523`). Command names are fuzzy-filtered before the first space. After the first space, Pi asks the matched command for optional argument completions (`packages/tui/src/autocomplete.ts:305`, `packages/tui/src/autocomplete.ts:355`).

Applying a command completion inserts `/<command> ` and moves the cursor after the trailing space. Argument completions and normal file completions use the same provider surface, but command-name completion is special-cased by the slash prefix (`packages/tui/src/autocomplete.ts:388`, `packages/tui/src/autocomplete.ts:401`).

## Interactive Built-Ins

The advertised built-ins live in `BUILTIN_SLASH_COMMANDS`: `settings`, `model`, `scoped-models`, `export`, `import`, `share`, `copy`, `name`, `session`, `changelog`, `hotkeys`, `fork`, `clone`, `tree`, `login`, `logout`, `new`, `compact`, `resume`, `reload`, and `quit` (`packages/coding-agent/src/core/slash-commands.ts:18`, `packages/coding-agent/src/core/slash-commands.ts:40`).

`InteractiveMode.setupEditorSubmitHandler()` checks for those commands before normal prompt submission. Most clear the editor, call a UI selector or handler, and return without involving the agent (`packages/coding-agent/src/modes/interactive/interactive-mode.ts:2464`, `packages/coding-agent/src/modes/interactive/interactive-mode.ts:2590`). After those checks, `!` and `!!` are treated as shell-command shortcuts, then compaction/streaming/normal prompt paths take over (`packages/coding-agent/src/modes/interactive/interactive-mode.ts:2593`, `packages/coding-agent/src/modes/interactive/interactive-mode.ts:2642`).

Because built-ins are interactive-mode checks, RPC documents that `get_commands` excludes them and that they do not execute if sent through `prompt` (`packages/coding-agent/docs/rpc.md:702`, `packages/coding-agent/docs/rpc.md:739`).

## Extension Commands

Extensions register slash commands with `pi.registerCommand(name, options)`. Registration stores the handler, description, optional argument-completion callback, and source info on the extension (`packages/coding-agent/src/core/extensions/loader.ts:201`, `packages/coding-agent/src/core/extensions/loader.ts:208`).

Duplicate extension command names are not overwritten. The runner resolves them to invocation names like `name:1`, `name:2` in insertion order, and command lookup uses the resolved invocation name (`packages/coding-agent/src/core/extensions/runner.ts:512`, `packages/coding-agent/src/core/extensions/runner.ts:558`). If an extension command conflicts with an interactive built-in, interactive autocomplete warns and skips it; in interactive submission, the built-in handler wins because it runs before `session.prompt()` (`packages/coding-agent/src/modes/interactive/interactive-mode.ts:433`, `packages/coding-agent/src/modes/interactive/interactive-mode.ts:502`, `packages/coding-agent/src/modes/interactive/interactive-mode.ts:2469`).

At prompt time, extension commands run first. `AgentSession.prompt()` checks `text.startsWith("/")`, parses the first space-delimited token as the command name, and calls the registered handler with the rest of the line as a raw args string (`packages/coding-agent/src/core/agent-session.ts:967`, `packages/coding-agent/src/core/agent-session.ts:976`, `packages/coding-agent/src/core/agent-session.ts:1117`, `packages/coding-agent/src/core/agent-session.ts:1140`). Handler errors are emitted through the extension error channel and still count as handled, so the text is not sent to the model (`packages/coding-agent/src/core/agent-session.ts:1129`, `packages/coding-agent/src/core/agent-session.ts:1139`).

Command handlers receive `ExtensionCommandContext`, which adds user-initiated session controls such as `waitForIdle`, `newSession`, `fork`, `navigateTree`, `switchSession`, and `reload` on top of the normal extension context (`packages/coding-agent/src/core/extensions/types.ts:333`, `packages/coding-agent/src/core/extensions/types.ts:364`, `packages/coding-agent/src/core/extensions/runner.ts:636`, `packages/coding-agent/src/core/extensions/runner.ts:668`).

## Skill Commands

`/skill:name args` is expanded inside `AgentSession.prompt()` after extension commands and input hooks. If the named skill exists, Pi reads the skill file, strips frontmatter, wraps the body in a `<skill name="..." location="...">` block, includes a note that references are relative to the skill base dir, and appends the raw args after the block (`packages/coding-agent/src/core/agent-session.ts:998`, `packages/coding-agent/src/core/agent-session.ts:1003`, `packages/coding-agent/src/core/agent-session.ts:1143`, `packages/coding-agent/src/core/agent-session.ts:1171`).

Unknown skills pass through unchanged. Read failures are emitted as extension-style errors and also pass the original text through (`packages/coding-agent/src/core/agent-session.ts:1155`, `packages/coding-agent/src/core/agent-session.ts:1170`).

One subtlety: interactive autocomplete respects `settingsManager.getEnableSkillCommands()`, but the core `AgentSession` expansion path checks the loaded skills directly. So the setting gates discovery in the TUI menu, not the existence of the core `/skill:name` expansion path (`packages/coding-agent/src/modes/interactive/interactive-mode.ts:504`, `packages/coding-agent/src/modes/interactive/interactive-mode.ts:516`, `packages/coding-agent/src/core/agent-session.ts:1148`, `packages/coding-agent/src/core/agent-session.ts:1171`).

## Prompt Templates

Prompt templates are Markdown files whose filename becomes a slash command. Pi loads description and `argument-hint` from frontmatter, with the first non-empty body line as a fallback description (`packages/coding-agent/src/core/prompt-templates.ts:104`, `packages/coding-agent/src/core/prompt-templates.ts:129`).

Expansion matches `/name args`, finds a template with that name, parses shell-ish quoted args, and substitutes `$1`, `$2`, `$@`, `$ARGUMENTS`, and slice forms like `${@:2}` into the template body (`packages/coding-agent/src/core/prompt-templates.ts:24`, `packages/coding-agent/src/core/prompt-templates.ts:55`, `packages/coding-agent/src/core/prompt-templates.ts:68`, `packages/coding-agent/src/core/prompt-templates.ts:101`, `packages/coding-agent/src/core/prompt-templates.ts:265`, `packages/coding-agent/src/core/prompt-templates.ts:285`).

If no template matches, the slash-prefixed text remains ordinary user text. There is no "unknown slash command" error at the core prompt layer.

## Queueing Semantics

When the agent is streaming, interactive Enter calls `session.prompt(text, { streamingBehavior: "steer" })`; Alt+Enter uses `"followUp"` (`packages/coding-agent/src/modes/interactive/interactive-mode.ts:2623`, `packages/coding-agent/src/modes/interactive/interactive-mode.ts:2631`, `packages/coding-agent/src/modes/interactive/interactive-mode.ts:3395`, `packages/coding-agent/src/modes/interactive/interactive-mode.ts:3424`).

`prompt()` still gives extension commands immediate execution even while streaming. For non-extension text, it expands skills/templates, then queues via steer or follow-up depending on `streamingBehavior` (`packages/coding-agent/src/core/agent-session.ts:967`, `packages/coding-agent/src/core/agent-session.ts:1019`).

The explicit `steer()` and `followUp()` APIs are stricter: they expand skill commands and prompt templates, but reject extension commands with an error because extension commands are meant to be executed via `prompt()` (`packages/coding-agent/src/core/agent-session.ts:1174`, `packages/coding-agent/src/core/agent-session.ts:1212`, `packages/coding-agent/src/core/agent-session.ts:1249`, `packages/coding-agent/src/core/agent-session.ts:1261`).

Extension-origin `sendUserMessage()` deliberately disables command handling and template expansion, so a command-looking string sent by an extension becomes literal user text. Pi has a regression test for this exact behavior: queued `/testcmd queued` from an extension does not dispatch `/testcmd` (`packages/coding-agent/src/core/agent-session.ts:1311`, `packages/coding-agent/src/core/agent-session.ts:1348`, `packages/coding-agent/test/suite/regressions/2023-queued-slash-command-followup.test.ts:17`, `packages/coding-agent/test/suite/regressions/2023-queued-slash-command-followup.test.ts:58`).

## RPC Surface

RPC `prompt` uses the same `AgentSession.prompt()` path, including extension-command execution, skill/template expansion, and streaming preflight success reporting (`packages/coding-agent/src/modes/rpc/rpc-mode.ts:389`, `packages/coding-agent/src/modes/rpc/rpc-mode.ts:410`). RPC `steer` and `follow_up` call the stricter queue APIs, so extension commands are not allowed there (`packages/coding-agent/src/modes/rpc/rpc-mode.ts:413`, `packages/coding-agent/src/modes/rpc/rpc-mode.ts:420`).

RPC `get_commands` returns extension commands, prompt templates, and skills in that order, with source metadata. Built-ins are omitted by design (`packages/coding-agent/src/modes/rpc/rpc-mode.ts:634`, `packages/coding-agent/src/modes/rpc/rpc-mode.ts:664`, `packages/coding-agent/docs/rpc.md:704`, `packages/coding-agent/docs/rpc.md:739`).

## Implications For Nav

1. Keep command discovery separate from command execution. Pi's autocomplete provider is a pure suggestion layer; dispatch happens later in interactive handlers or `AgentSession.prompt()`.
2. Treat built-in UI commands as TUI-owned behavior. If nav copies this shape, `/model`, `/settings`, `/resume`, and similar commands should be handled before JSON-RPC/SSE prompt submission, while reusable prompt commands should live behind a backend/session command API.
3. Preserve source/provenance metadata for commands. Pi exposes `sourceInfo` for extension, prompt, and skill commands, which is useful for autocomplete labels, RPC listing, and conflict diagnostics.
4. Make queue semantics explicit. Pi's split between immediate extension commands, steer/follow-up user messages, and literal extension-origin `sendUserMessage()` avoids accidental command dispatch from queued text.
5. Decide whether unknown slash commands are user text or errors. Pi silently sends unknown slash-prefixed text to the model. That is forgiving, but nav may want an explicit "unknown command" UI if users expect command-mode behavior.
