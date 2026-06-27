# AI Elements v7 Wiring Plan

Date: 2026-06-27

## Goal

Move Nav's desktop chat UI onto AI SDK v7-compatible AI Elements rendering, then wire the existing `reasoning.tsx` and `task.tsx` components against real message parts/events instead of flattening everything into assistant text.

This is a UI/rendering and dependency-alignment slice. It is not the full runtime migration away from Flue.

## Current Baseline

- The desktop app currently renders chat from `@flue/react` via `useFlueAgent`.
- `packages/desktop/package.json` currently pins `ai` to `6.0.209`.
- The npm `ai` package latest dist-tag is `7.0.3`; v6 is now on the `ai-v6` dist-tag.
- `packages/desktop/src/main.tsx` currently collapses `text` and `reasoning` parts into a single string through `getMessageText`.
- The generated AI Elements files under `packages/desktop/src/components/ai-elements` should not be manually modified unless a component-level change is explicitly required.

## Non-Goals

- Do not replace Flue as the runtime in this slice.
- Do not introduce a new chat persistence model.
- Do not fake task UI from ordinary assistant prose.
- Do not broaden this into a sandbox or harness migration.

## Phase 1: Confirm and Protect the Worktree

1. Inspect `git status --short --branch`.
2. Identify unrelated local changes and avoid touching them.
3. Confirm the current desktop runtime path:
   - `packages/desktop/src/main.tsx`
   - `packages/desktop/package.json`
   - `packages/flue/.flue/agents/nav.ts`
4. Re-check the npm dist-tag before changing dependencies:
   - `pnpm view ai version dist-tags --json`

## Phase 2: Upgrade AI SDK to v7

1. Update `packages/desktop/package.json`:
   - from `ai: 6.0.209`
   - to `ai: 7.0.3`
2. Run install to update `pnpm-lock.yaml`.
3. Do not update unrelated packages unless the v7 install requires it.
4. Verify Node compatibility. AI SDK v7 requires Node 22+; this repo already targets Node 24.

## Phase 3: Fix AI Elements v7 Type Drift

Run the desktop build/typecheck and fix only actual breakage.

Expected local changes:

1. Update usage-token fields in `packages/desktop/src/components/ai-elements/context.tsx`:
   - `usage.reasoningTokens` to `usage.outputTokenDetails.reasoningTokens`
   - `usage.cachedInputTokens` to `usage.inputTokenDetails.cacheReadTokens`
2. Prefer stable v7 type names where the generated components currently import experimental aliases:
   - `Experimental_SpeechResult` to `SpeechResult`
   - `Experimental_TranscriptionResult` to `TranscriptionResult`
   - Review `Experimental_GeneratedImage` and replace with the correct stable v7 generated-file/image type only if the build requires it.
3. Leave still-compatible type imports alone:
   - `UIMessage`
   - `TextUIPart`
   - `ReasoningUIPart`
   - `ToolUIPart`
   - `DynamicToolUIPart`
   - `ChatStatus`
   - `FileUIPart`

## Phase 4: Add a Real Message Parts Renderer

Replace `getMessageText(message)` usage in `packages/desktop/src/main.tsx` with a renderer that preserves `message.parts`.

Preferred shape:

- Add a small non-generated component, for example:
  - `packages/desktop/src/components/chat-message-parts.tsx`
- Keep `main.tsx` focused on app wiring and layout.
- Render parts in order.

Initial part mapping:

| Part type | UI |
| --- | --- |
| `text` | `MessageResponse` |
| `reasoning` | `Reasoning`, `ReasoningTrigger`, `ReasoningContent` |
| `dynamic-tool` | `Tool`, `ToolHeader`, `ToolContent`, `ToolInput`, `ToolOutput` |
| `tool-*` | Same tool renderer, if/when AI SDK static tool parts appear |
| `file` | Minimal safe fallback or existing file/media component if already compatible |
| `data-*` | Ignore unknown data parts or render a small debug-safe fallback only when useful |

## Phase 5: Wire Reasoning

For every `reasoning` part:

```tsx
<Reasoning isStreaming={part.state === "streaming"}>
  <ReasoningTrigger />
  <ReasoningContent>{part.text}</ReasoningContent>
</Reasoning>
```

Behavior expectations:

- Reasoning is no longer concatenated into final assistant text.
- Streaming reasoning auto-opens.
- Completed reasoning auto-closes according to the existing component behavior.
- Empty reasoning parts should not render visible empty chrome.

## Phase 6: Wire Tool Progress as the First Task-Like Surface

Flue currently exposes useful work progress as `dynamic-tool` parts. Use those before inventing task data.

For each `dynamic-tool` part:

```tsx
<Tool defaultOpen={part.state !== "output-available"}>
  <ToolHeader
    state={part.state}
    toolName={part.toolName}
    type={part.type}
  />
  <ToolContent>
    <ToolInput input={part.input} />
    <ToolOutput errorText={part.errorText} output={part.output} />
  </ToolContent>
</Tool>
```

This gives the user real running/completed/error feedback without pretending it is a formal task list.

## Phase 7: Wire `task.tsx` Only From Real Task Data

Do not infer tasks from normal text.

Use `task.tsx` when one of these is true:

1. The backend emits `data-task` / `data-task-item` UI message parts.
2. The desktop app gains a small event bridge that exposes Flue `task_start` and `task` events as UI-renderable data.

Preferred route:

- Emit task progress as `data-task` parts so the UI stays centered on `UIMessage.parts`.
- Minimal data shape:

```ts
type NavTaskData = {
  id: string;
  title: string;
  status: "running" | "completed" | "error";
  cwd?: string;
  durationMs?: number;
  result?: unknown;
  error?: unknown;
};
```

Then render:

```tsx
<Task defaultOpen={task.status === "running"}>
  <TaskTrigger title={task.title} />
  <TaskContent>
    {task.cwd ? <TaskItemFile>{task.cwd}</TaskItemFile> : null}
    <TaskItem>{task.status}</TaskItem>
  </TaskContent>
</Task>
```

## Phase 8: Validation

Run the narrow validation bundle first:

```sh
pnpm run format
pnpm run lint
pnpm --filter @nav/desktop build
```

If the dependency upgrade affects broader workspace types or lockfile behavior, widen to:

```sh
pnpm run test
pnpm --filter @nav/desktop test
```

For UI confidence, run the app and verify a real agent interaction:

1. Assistant final text renders normally.
2. Reasoning renders in the collapsible reasoning component.
3. Tool calls render separately from Markdown text.
4. No empty reasoning/task chrome appears.
5. The prompt composer still disables/enables correctly during submitted and streaming states.

## Risks

- `@flue/react` has its own `UIMessage` type, not the AI SDK exported type, even though the shapes overlap. The renderer should type against the actual messages it receives and narrow by `part.type`.
- AI SDK v7 usage metadata changed shape. Context/token components are the likely first build failures.
- `task.tsx` needs real structured task data. Wiring it prematurely would create misleading UI.

## Recommended First Implementation Slice

1. Upgrade `ai` to `7.0.3`.
2. Fix v7 build errors in AI Elements.
3. Add `ChatMessageParts`.
4. Render `text`, `reasoning`, and `dynamic-tool`.
5. Leave `task.tsx` behind a real `data-task` follow-up.

This gives visible value quickly while keeping the runtime migration separate.
