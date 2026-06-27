import type { UIMessage, UIMessagePart } from "@flue/react";
import {
  BrainIcon,
  ChevronDownIcon,
  FileTextIcon,
  PencilIcon,
  SearchIcon,
  TerminalIcon,
  WrenchIcon,
} from "lucide-react";
import { useEffect, useRef, useState } from "react";

import { MessageResponse } from "@/components/ai-elements/message";
import {
  Task,
  TaskContent,
  TaskItem,
  TaskTrigger,
} from "@/components/ai-elements/task";

type TextPart = Extract<UIMessagePart, { type: "text" }>;
type ReasoningPart = Extract<UIMessagePart, { type: "reasoning" }>;
type DynamicToolPart = Extract<UIMessagePart, { type: "dynamic-tool" }>;
type ActivityPart = ReasoningPart | DynamicToolPart;
type KeyedActivityPart = { key: string; part: ActivityPart };
type ActivitySummaryKind =
  | "command"
  | "edit"
  | "other"
  | "read"
  | "reasoning"
  | "search";
type ActivitySummary = {
  kind: ActivitySummaryKind;
  title: string;
};

type RenderItem =
  | { type: "text"; key: string; part: TextPart }
  | {
      type: "activity";
      key: string;
      parts: KeyedActivityPart[];
    };

const isActivityPart = (part: UIMessagePart): part is ActivityPart =>
  part.type === "reasoning" || part.type === "dynamic-tool";

const isActiveActivityPart = (part: ActivityPart) =>
  (part.type === "reasoning" && part.state === "streaming") ||
  (part.type === "dynamic-tool" && part.state === "input-available");

const COMMAND_TOOL_NAMES = new Set([
  "bash",
  "command",
  "exec",
  "exec_command",
  "run_command",
  "shell",
]);
const EDIT_TOOL_NAMES = new Set([
  "apply_patch",
  "delete",
  "edit",
  "write",
  "write_file",
]);
const READ_TOOL_NAMES = new Set(["cat", "read", "read_file"]);
const SEARCH_TOOL_NAMES = new Set([
  "find",
  "glob",
  "grep",
  "list",
  "ls",
  "rg",
  "search",
]);

const capitalize = (text: string) =>
  text.charAt(0).toUpperCase() + text.slice(1);

const joinPhrases = (phrases: string[]) => {
  if (phrases.length <= 1) {
    return phrases[0] ?? "";
  }

  if (phrases.length === 2) {
    return `${phrases[0]} and ${phrases[1]}`;
  }

  return `${phrases.slice(0, -1).join(", ")}, and ${phrases.at(-1)}`;
};

const normalizeToolName = (name: string) =>
  name.trim().toLowerCase().replaceAll("-", "_");

const getToolKind = (toolName: string): ActivitySummaryKind => {
  const name = normalizeToolName(toolName);

  if (EDIT_TOOL_NAMES.has(name)) {
    return "edit";
  }

  if (READ_TOOL_NAMES.has(name)) {
    return "read";
  }

  if (SEARCH_TOOL_NAMES.has(name)) {
    return "search";
  }

  if (COMMAND_TOOL_NAMES.has(name)) {
    return "command";
  }

  return "other";
};

const countToolKinds = (parts: KeyedActivityPart[]) =>
  parts.reduce<Record<ActivitySummaryKind, number>>(
    (counts, { part }) => {
      if (part.type === "reasoning") {
        counts.reasoning += 1;
        return counts;
      }

      counts[getToolKind(part.toolName)] += 1;
      return counts;
    },
    {
      command: 0,
      edit: 0,
      other: 0,
      read: 0,
      reasoning: 0,
      search: 0,
    },
  );

const createActivitySummary = (parts: KeyedActivityPart[]): ActivitySummary => {
  const counts = countToolKinds(parts);
  const phrases: string[] = [];

  if (counts.edit > 0) {
    phrases.push(
      counts.edit === 1 ? "Edited a file" : `Edited ${counts.edit} files`,
    );
  }

  if (counts.read > 0) {
    phrases.push(
      counts.read === 1 ? "Read a file" : `Read ${counts.read} files`,
    );
  }

  if (counts.search > 0) {
    phrases.push("searched code");
  }

  if (counts.other > 0) {
    phrases.push(
      counts.other === 1 ? "used a tool" : `used ${counts.other} tools`,
    );
  }

  const commandPhrase =
    counts.command > 0
      ? `ran ${counts.command} command${counts.command === 1 ? "" : "s"}`
      : "";
  const nonCommandSummary = joinPhrases(phrases);
  const title =
    nonCommandSummary && commandPhrase
      ? `${nonCommandSummary}, ${commandPhrase}`
      : nonCommandSummary || capitalize(commandPhrase);

  if (title) {
    return {
      kind:
        counts.edit > 0
          ? "edit"
          : counts.command > 0
            ? "command"
            : counts.read > 0
              ? "read"
              : counts.search > 0
                ? "search"
                : "other",
      title,
    };
  }

  return {
    kind: "reasoning" as const,
    title: "Thought through the task",
  };
};

const getToolActionLabel = (toolName: string) => {
  switch (getToolKind(toolName)) {
    case "command":
      return "Ran command";
    case "edit":
      return "Edited file";
    case "read":
      return "Read file";
    case "search":
      return "Searched code";
    case "other":
    case "reasoning":
      return `Used ${toolName}`;
  }
};

const getToolDetail = (part: DynamicToolPart) => {
  const action = getToolActionLabel(part.toolName);

  switch (part.state) {
    case "input-available":
      return `${action} running`;
    case "output-error":
      return part.errorText
        ? `${action} failed: ${part.errorText}`
        : `${action} failed`;
    default:
      return action;
  }
};

function ActivitySummaryIcon({ kind }: { kind: ActivitySummaryKind }) {
  switch (kind) {
    case "command":
      return <TerminalIcon className="size-4 shrink-0" />;
    case "edit":
      return <PencilIcon className="size-4 shrink-0" />;
    case "read":
      return <FileTextIcon className="size-4 shrink-0" />;
    case "search":
      return <SearchIcon className="size-4 shrink-0" />;
    case "reasoning":
      return <BrainIcon className="size-4 shrink-0" />;
    case "other":
      return <WrenchIcon className="size-4 shrink-0" />;
  }
}

// Renders a Flue UIMessage by walking its parts in order instead of flattening
// everything into assistant text: `text` becomes the markdown answer, and
// thinking/tool parts share one stable Task block. Narrow by `part.type`
// — these are @flue/react's part types, which only overlap with the AI SDK
// types the generated components are written against.
function TextPartView({ part }: { part: TextPart }) {
  return part.text ? <MessageResponse>{part.text}</MessageResponse> : null;
}

function ActivityPartView({ part }: { part: ActivityPart }) {
  switch (part.type) {
    case "reasoning":
      // Don't render empty reasoning chrome (e.g. a started-but-empty block).
      return part.text.trim() ? (
        <TaskItem>
          <MessageResponse>{part.text}</MessageResponse>
        </TaskItem>
      ) : null;

    case "dynamic-tool":
      return <TaskItem>{getToolDetail(part)}</TaskItem>;
  }
}

function ActivityView({
  isLatestMessage,
  parts,
}: {
  isLatestMessage: boolean;
  parts: KeyedActivityPart[];
}) {
  const isActive = parts.some(({ part }) => isActiveActivityPart(part));
  const shouldAutoOpen = isLatestMessage && isActive;
  const [isOpen, setIsOpen] = useState(shouldAutoOpen);
  const hasUserToggledRef = useRef(false);

  useEffect(() => {
    if (hasUserToggledRef.current) {
      return;
    }

    if (shouldAutoOpen) {
      setIsOpen(true);
    } else if (!isLatestMessage) {
      setIsOpen(false);
    }
  }, [isLatestMessage, shouldAutoOpen]);

  const handleOpenChange = (open: boolean) => {
    hasUserToggledRef.current = true;
    setIsOpen(open);
  };
  const summary = createActivitySummary(parts);

  return (
    <Task
      className="not-prose w-full"
      defaultOpen={shouldAutoOpen}
      onOpenChange={handleOpenChange}
      open={isOpen}
    >
      <TaskTrigger title={summary.title}>
        <div className="flex w-full cursor-pointer items-center gap-2 text-muted-foreground text-sm transition-colors hover:text-foreground">
          <ActivitySummaryIcon kind={summary.kind} />
          <p className="min-w-0 flex-1 truncate text-sm">{summary.title}</p>
          <ChevronDownIcon className="size-4 shrink-0 transition-transform group-data-[state=open]:rotate-180" />
        </div>
      </TaskTrigger>
      <TaskContent>
        {parts.map(({ key, part }) => (
          <ActivityPartView key={key} part={part} />
        ))}
      </TaskContent>
    </Task>
  );
}

function createMessageRenderItems(parts: UIMessagePart[]) {
  // Parts are positionally stable (appended and mutated in place, never
  // reordered), so a per-type ordinal is a stable key without leaning on the
  // raw array index.
  const seen: Record<string, number> = {};
  const renderItems: RenderItem[] = [];
  let activityItem: Extract<RenderItem, { type: "activity" }> | undefined;

  const ensureActivityItem = () => {
    if (!activityItem) {
      activityItem = {
        key: "activity",
        parts: [],
        type: "activity",
      };
      renderItems.push(activityItem);
    }

    return activityItem;
  };

  for (const part of parts) {
    seen[part.type] = (seen[part.type] ?? 0) + 1;
    const key =
      part.type === "dynamic-tool"
        ? part.toolCallId
        : `${part.type}-${seen[part.type]}`;

    if (part.type === "text") {
      if (part.text) {
        renderItems.push({ key, part, type: "text" });
      }
      continue;
    }

    if (isActivityPart(part)) {
      if (part.type === "reasoning" && !part.text.trim()) {
        continue;
      }

      ensureActivityItem().parts.push({ key, part });
    }
  }

  return renderItems;
}

export function hasRenderableMessageParts(parts: UIMessagePart[]) {
  return createMessageRenderItems(parts).length > 0;
}

export function ChatMessageParts({
  isLatestMessage,
  message,
}: {
  isLatestMessage: boolean;
  message: UIMessage;
}) {
  const renderItems = createMessageRenderItems(message.parts);

  return (
    <>
      {renderItems.map((item) => (
        <MessageRenderItem
          isLatestMessage={isLatestMessage}
          item={item}
          key={item.key}
        />
      ))}
    </>
  );
}

function MessageRenderItem({
  isLatestMessage,
  item,
}: {
  isLatestMessage: boolean;
  item: RenderItem;
}) {
  switch (item.type) {
    case "text":
      return <TextPartView part={item.part} />;
    case "activity":
      return (
        <ActivityView isLatestMessage={isLatestMessage} parts={item.parts} />
      );
  }
}
