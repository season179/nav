# Claude Code 2.1.88 Slash Commands Research

Source project: `/Users/season/Personal/claude-code-2.1.88` at branch `main`, commit `c8cd253`, clean worktree. Citations are repo-relative paths under `/Users/season/Personal/claude-code-2.1.88/source/src`.

## Executive Summary

Claude Code has one unified slash-command registry, but three execution shapes:

1. `prompt` commands expand into hidden model-facing prompt content. This covers Markdown skills, legacy `.claude/commands`, plugin commands, plugin skills, bundled skills, workflow commands, and some built-ins such as `/init` (`types/command.ts:25`, `commands.ts:445`, `commands.ts:460`).
2. `local` commands run lazy-loaded local TypeScript and return `text`, `compact`, or `skip` without invoking the model unless their result later asks for it. `/compact` is in this family (`types/command.ts:62`, `utils/processUserInput/processSlashCommand.tsx:657`).
3. `local-jsx` commands lazy-load an Ink/React UI, receive an `onDone` callback, and can render transient UI such as pickers or settings panels. Commands such as `/model` and `/exit` use this shape (`types/command.ts:117`, `types/command.ts:131`, `utils/processUserInput/processSlashCommand.tsx:551`).

The main design: slash commands are not a separate parser bolted onto the TUI. They are command objects with shared metadata, loaded once and reused for dispatch, autocomplete, help, non-interactive mode, model-invoked Skill tooling, and remote safety filtering.

## Command Object Model

The shared `CommandBase` carries display and policy metadata: `name`, `aliases`, `description`, `isHidden`, `argumentHint`, `whenToUse`, `disableModelInvocation`, `userInvocable`, `loadedFrom`, `kind`, `immediate`, `isSensitive`, and `userFacingName()` (`types/command.ts:175`).

The execution-specific fields sit beside that base:

- `PromptCommand` has `getPromptForCommand(args, context)`, optional `allowedTools`, `model`, `effort`, `context: 'inline' | 'fork'`, `agent`, hooks, path filters, and source metadata (`types/command.ts:25`).
- `LocalCommand` has `supportsNonInteractive` and a lazy `load()` that returns `call(args, context)` (`types/command.ts:62`, `types/command.ts:74`).
- `LocalJSXCommand` has a lazy `load()` returning `call(onDone, context, args)` and can return a React node for Ink rendering (`types/command.ts:117`, `types/command.ts:131`, `types/command.ts:144`).

Name resolution accepts the internal `name`, the display `userFacingName()`, and any alias (`commands.ts:688`). This is why the same registry can serve typed slash input, typeahead display names, and built-in aliases.

## Discovery And Ordering

Built-ins are declared as a memoized `COMMANDS()` list containing local, local-jsx, and prompt commands (`commands.ts:258`). `builtInCommandNames` includes both command names and aliases for telemetry and classification (`commands.ts:348`).

`loadAllCommands(cwd)` loads command sources in parallel, then returns them in this order:

1. bundled skills
2. built-in plugin skills
3. skills from managed/user/project/additional `.claude/skills`
4. workflow commands
5. plugin commands
6. plugin skills
7. built-in commands

That order is explicit in `commands.ts:449` and `commands.ts:460`. `getCommands(cwd)` then filters by `availability` and `isEnabled()`, and inserts dynamic skills just before built-ins when present (`commands.ts:476`, `commands.ts:482`, `commands.ts:504`).

Startup registers bundled plugins/skills before kicking off command loading, then overlaps `getCommands(preSetupCwd)` with setup and joins it later (`main.tsx:1918`, `main.tsx:1924`, `main.tsx:1928`, `main.tsx:2022`).

The CLI flag `--disable-slash-commands` is described as disabling all skills, but the implementation empties the whole command list in both interactive and headless paths (`main.tsx:1006`, `main.tsx:1133`, `screens/REPL.tsx:831`, `main.tsx:2620`).

## Parsing And Dispatch

Parsing is deliberately simple. `parseSlashCommand(input)` trims the string, requires a leading `/`, takes the first space-delimited token as the command name, and leaves the rest of the line as raw args. A special second token `(MCP)` is folded into the command name and sets `isMcp` (`utils/slashCommandParsing.ts:25`, `utils/slashCommandParsing.ts:41`, `utils/slashCommandParsing.ts:45`, `utils/slashCommandParsing.ts:52`).

Normal user input goes through `processUserInput`. Slash commands skip normal attachment extraction because prompt command expansion extracts attachments later from the expanded command body (`utils/processUserInput/processUserInput.ts:495`). If the effective input starts with `/`, `processUserInput` lazy-imports and calls `processSlashCommand` (`utils/processUserInput/processUserInput.ts:531`).

Unknown slash input has a useful split:

- If the first token looks like a command name, using only `[a-zA-Z0-9:_-]`, and is not an existing absolute path, it returns a UI error `Unknown skill: <name>` without querying the model (`utils/processUserInput/processSlashCommand.tsx:304`, `utils/processUserInput/processSlashCommand.tsx:332`, `utils/processUserInput/processSlashCommand.tsx:343`).
- If the slash-prefixed input looks path-like or is an actual absolute path, it falls back to a normal model prompt instead of command failure (`utils/processUserInput/processSlashCommand.tsx:362`, `utils/processUserInput/processSlashCommand.tsx:371`).

Valid commands are routed through `getMessagesForSlashCommand`, which switches on `command.type` (`utils/processUserInput/processSlashCommand.tsx:395`, `utils/processUserInput/processSlashCommand.tsx:525`, `utils/processUserInput/processSlashCommand.tsx:550`).

## Prompt Commands And Skills

Prompt commands are the core reusable slash-command abstraction. When invoked, Claude Code calls `command.getPromptForCommand(args, context)`, wraps a visible command-loading message, puts the expanded prompt body in a hidden `isMeta` user message, extracts attachments from that body, and adds a `command_permissions` attachment with the command's extra tools and optional model override (`utils/processUserInput/processSlashCommand.tsx:827`, `utils/processUserInput/processSlashCommand.tsx:869`, `utils/processUserInput/processSlashCommand.tsx:886`, `utils/processUserInput/processSlashCommand.tsx:902`).

Prompt commands can be user-invocable or model-only. If `userInvocable === false`, direct `/name` input is rejected with a message telling the user to ask Claude to use that skill, while model-side Skill tooling can still list/invoke it unless `disableModelInvocation` excludes it (`utils/processUserInput/processSlashCommand.tsx:533`, `commands.ts:561`, `commands.ts:586`).

File-based skills parse frontmatter fields including description, `allowed-tools`, `argument-hint`, named `arguments`, `when_to_use`, `version`, `model`, `effort`, `disable-model-invocation`, `user-invocable`, hooks, `context: fork`, `agent`, and `shell` (`skills/loadSkillsDir.ts:185`, `skills/loadSkillsDir.ts:237`). They become `prompt` commands through `createSkillCommand` (`skills/loadSkillsDir.ts:270`, `skills/loadSkillsDir.ts:317`).

Argument substitution is shell-quote aware for the later prompt-expansion phase. It supports `$ARGUMENTS`, `$ARGUMENTS[0]`, shorthand `$0`, named arguments from frontmatter, and appends `ARGUMENTS: ...` when no placeholder exists (`utils/argumentSubstitution.ts:1`, `utils/argumentSubstitution.ts:24`, `utils/argumentSubstitution.ts:94`, `utils/argumentSubstitution.ts:123`, `utils/argumentSubstitution.ts:138`).

Prompt bodies may include shell interpolation using inline bang-backtick forms or fenced bang blocks. Before executing those, Claude Code checks tool permissions, runs through Bash or PowerShell, and replaces the marker with stored tool output (`utils/promptShellExecution.ts:48`, `utils/promptShellExecution.ts:69`, `utils/promptShellExecution.ts:97`, `utils/promptShellExecution.ts:115`). MCP skills are explicitly excluded from this shell-execution path because they are remote/untrusted (`skills/loadSkillsDir.ts:371`).

Skills from `.claude/skills` must use `skill-name/SKILL.md`; loose `.md` files in `skills/` are ignored (`skills/loadSkillsDir.ts:403`, `skills/loadSkillsDir.ts:424`). Legacy `.claude/commands` still supports both single `.md` files and directories with `SKILL.md`, using nested directories as `:` namespaces (`skills/loadSkillsDir.ts:482`, `skills/loadSkillsDir.ts:523`, `skills/loadSkillsDir.ts:561`).

Project discovery walks from cwd up to the git root or home boundary, which prevents a parent `.claude/commands` from leaking into an unrelated git repository (`utils/markdownConfigLoader.ts:226`, `utils/markdownConfigLoader.ts:234`, `utils/markdownConfigLoader.ts:267`). Managed, user, and project Markdown files are loaded with priority managed > user > project and deduped by file identity (`utils/markdownConfigLoader.ts:297`, `utils/markdownConfigLoader.ts:337`, `utils/markdownConfigLoader.ts:377`).

Plugin commands use the same `prompt` shape. Regular plugin command files are named `pluginName[:namespace]:file`, while `SKILL.md` uses the parent directory name (`utils/plugins/loadPluginCommands.ts:60`, `utils/plugins/loadPluginCommands.ts:67`, `utils/plugins/loadPluginCommands.ts:82`). Plugin command frontmatter mirrors skill fields, adds plugin-specific variable substitution such as `${CLAUDE_PLUGIN_ROOT}`, `${CLAUDE_PLUGIN_DATA}`, `${user_config.X}`, and can be loaded from default command dirs, extra manifest paths, single markdown files, or inline manifest content (`utils/plugins/loadPluginCommands.ts:218`, `utils/plugins/loadPluginCommands.ts:241`, `utils/plugins/loadPluginCommands.ts:326`, `utils/plugins/loadPluginCommands.ts:414`, `utils/plugins/loadPluginCommands.ts:465`, `utils/plugins/loadPluginCommands.ts:603`).

## Local And Local-JSX Commands

Local commands lazy-load their module, call `mod.call(args, context)`, and map the result into local transcript messages. `skip` produces no messages, `compact` builds post-compaction messages, and `text` becomes `<local-command-stdout>...` without model query (`utils/processUserInput/processSlashCommand.tsx:657`, `utils/processUserInput/processSlashCommand.tsx:668`, `utils/processUserInput/processSlashCommand.tsx:670`, `utils/processUserInput/processSlashCommand.tsx:679`, `utils/processUserInput/processSlashCommand.tsx:707`).

Local-jsx commands are async UI commands. They get `onDone(result, options)` and may return a React node. `onDone` can skip display, add hidden model-visible meta messages, request a model query, or enqueue/prefill a next input (`types/command.ts:117`, `utils/processUserInput/processSlashCommand.tsx:553`, `utils/processUserInput/processSlashCommand.tsx:563`, `utils/processUserInput/processSlashCommand.tsx:575`, `utils/processUserInput/processSlashCommand.tsx:592`). If a JSX command returns UI in an interactive session, the REPL calls `setToolJSX` and hides the prompt input while the command UI is active (`utils/processUserInput/processSlashCommand.tsx:609`, `utils/processUserInput/processSlashCommand.tsx:630`).

Some local-jsx commands opt into `immediate: true`. While a query is active, immediate commands and keybinding-triggered commands bypass the normal queue and execute immediately, but only for `local-jsx` commands (`screens/REPL.tsx:3158`, `screens/REPL.tsx:3170`, `screens/REPL.tsx:3184`). This is the mechanism that lets UI commands such as mode/picker controls run during streaming without waiting for the model turn to finish.

## Autocomplete And Help

Slash autocomplete uses the same command registry. It only activates for input starting with `/`, and once there are real arguments it stops suggesting command names (`utils/suggestions/commandSuggestions.ts:198`, `utils/suggestions/commandSuggestions.ts:208`, `utils/suggestions/commandSuggestions.ts:292`, `utils/suggestions/commandSuggestions.ts:301`).

For a bare `/`, visible commands are grouped as recently used prompt commands, built-ins, user commands, project commands, policy commands, then other commands; each group is alphabetized (`utils/suggestions/commandSuggestions.ts:308`, `utils/suggestions/commandSuggestions.ts:312`, `utils/suggestions/commandSuggestions.ts:331`, `utils/suggestions/commandSuggestions.ts:360`, `utils/suggestions/commandSuggestions.ts:370`).

Suggestion IDs include command source/repository for prompt commands, so same display names from different sources can coexist in the UI (`utils/suggestions/commandSuggestions.ts:225`, `utils/suggestions/commandSuggestions.ts:233`). Suggestion display uses `userFacingName()`, matched aliases, source-formatted descriptions, workflow badges, and prompt arg-name annotations (`utils/suggestions/commandSuggestions.ts:265`, `utils/suggestions/commandSuggestions.ts:273`).

Typeahead gives slash commands priority over `@` mention suggestions in prompt mode. It special-cases `/add-dir` directory completions and `/resume` title completions, then shows either static `argumentHint` or progressive hints from prompt command `argNames` (`hooks/useTypeahead.tsx:655`, `hooks/useTypeahead.tsx:661`, `hooks/useTypeahead.tsx:692`, `hooks/useTypeahead.tsx:729`, `hooks/useTypeahead.tsx:751`).

## Queueing, Headless, And Remote Safety

Queued commands are processed uniformly through `processUserInput`. Attachments, IDE selection, and pasted content are applied only to the first command in a batch to avoid duplicating turn-level context (`utils/handlePromptSubmit.ts:448`, `utils/handlePromptSubmit.ts:473`, `utils/handlePromptSubmit.ts:482`, `utils/handlePromptSubmit.ts:491`). The first command's `shouldQuery`, `allowedTools`, `model`, `effort`, and next-input controls govern the call into `onQuery` (`utils/handlePromptSubmit.ts:514`, `utils/handlePromptSubmit.ts:559`, `utils/handlePromptSubmit.ts:588`).

The queue only drains when no query is active, there is queued input, and no local JSX UI is blocking input (`hooks/useQueueProcessor.ts:23`, `hooks/useQueueProcessor.ts:48`).

Headless mode supports all prompt commands except those marked `disableNonInteractive`, plus local commands that opt into `supportsNonInteractive`. Local-jsx commands are excluded from headless execution (`main.tsx:2620`).

Remote and bridge modes have explicit safety filters. Remote mode shows only a curated `REMOTE_SAFE_COMMANDS` set of local TUI-safe commands (`commands.ts:610`, `commands.ts:619`). Bridge input allows all prompt commands, blocks all local-jsx commands, and allows only explicitly listed local commands (`commands.ts:639`, `commands.ts:651`, `commands.ts:662`).

## Implications For Nav

1. Use one command registry as the source of truth. Claude Code gets a lot of leverage because dispatch, autocomplete, help, headless filtering, and model-invoked skills all read the same command objects.
2. Keep command effects typed. The `prompt` / `local` / `local-jsx` split is clean: model-context expansion, local state/text effects, and interactive UI are separate execution contracts.
3. Treat user-created Markdown commands as prompt commands, not local code. Claude Code lets Markdown define model instructions, permissions, model/effort overrides, argument hints, and optional shell interpolation, while local TypeScript remains reserved for trusted built-ins/plugins.
4. Be explicit about remote safety. The bridge allowlist is a good pattern: prompt expansion is safe to relay, local text commands need opt-in, and terminal UI commands should be blocked.
5. Decide unknown-slash behavior early. Claude Code reports command-looking unknown input as `Unknown skill`, but lets path-like slash input become ordinary prompt text. That is forgiving for absolute paths while still helping users who typo a real command.
6. If nav adds interactive slash UIs, make them immediate only when they are purely local. Claude Code's immediate path is intentionally restricted to `local-jsx`, which avoids mid-turn model-pipeline surprises.
