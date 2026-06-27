import type { UIMessage, UIMessagePart } from "@flue/react";
import { WrenchIcon } from "lucide-react";
import { useEffect, useState } from "react";

import {
  ChainOfThought,
  ChainOfThoughtContent,
  ChainOfThoughtHeader,
  ChainOfThoughtStep,
} from "@/components/ai-elements/chain-of-thought";
import { MessageResponse } from "@/components/ai-elements/message";
import {
  Tool,
  ToolContent,
  ToolHeader,
  ToolInput,
  ToolOutput,
} from "@/components/ai-elements/tool";

type TextPart = Extract<UIMessagePart, { type: "text" }>;
type ReasoningPart = Extract<UIMessagePart, { type: "reasoning" }>;
type DynamicToolPart = Extract<UIMessagePart, { type: "dynamic-tool" }>;
type ActivityPart = ReasoningPart | DynamicToolPart;
type KeyedActivityPart = { key: string; part: ActivityPart };

type RenderItem =
  | { type: "text"; key: string; part: TextPart }
  | {
      type: "activity";
      key: string;
      parts: KeyedActivityPart[];
      hasFollowingText: boolean;
    };

const isActivityPart = (part: UIMessagePart): part is ActivityPart =>
  part.type === "reasoning" || part.type === "dynamic-tool";

const isActiveActivityPart = (part: ActivityPart) =>
  (part.type === "reasoning" && part.state === "streaming") ||
  (part.type === "dynamic-tool" && part.state === "input-available");

const toolStepStatus = (
  state: DynamicToolPart["state"],
): "active" | "complete" | "pending" =>
  state === "input-available" ? "active" : "complete";

// Renders a Flue UIMessage by walking its parts in order instead of flattening
// everything into assistant text: `text` becomes the markdown answer, and
// adjacent thinking/tool parts become one Activity block. Narrow by `part.type`
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
        <MessageResponse className="text-muted-foreground text-sm">
          {part.text}
        </MessageResponse>
      ) : null;

    case "dynamic-tool": {
      const output =
        part.state === "output-available" ? part.output : undefined;
      const errorText =
        part.state === "output-error" ? part.errorText : undefined;

      return (
        <ChainOfThoughtStep
          icon={WrenchIcon}
          label={<span>{part.toolName}</span>}
          status={toolStepStatus(part.state)}
        >
          <Tool
            className="mt-2 mb-0 border-border/70 bg-background/70"
            defaultOpen={
              part.state === "input-available" || part.state === "output-error"
            }
          >
            <ToolHeader
              className="p-2 text-xs"
              state={part.state}
              title="Details"
              toolName={part.toolName}
              type={part.type}
            />
            <ToolContent className="space-y-3 border-t p-3">
              <ToolInput input={part.input} />
              <ToolOutput errorText={errorText} output={output} />
            </ToolContent>
          </Tool>
        </ChainOfThoughtStep>
      );
    }
  }
}

function ActivityView({
  hasFollowingText,
  isLatestMessage,
  parts,
}: {
  hasFollowingText: boolean;
  isLatestMessage: boolean;
  parts: KeyedActivityPart[];
}) {
  const isActive = parts.some(({ part }) => isActiveActivityPart(part));
  const shouldBeOpen = isLatestMessage && isActive && !hasFollowingText;
  const [isOpen, setIsOpen] = useState(shouldBeOpen);

  useEffect(() => {
    setIsOpen(shouldBeOpen);
  }, [shouldBeOpen]);

  return (
    <ChainOfThought onOpenChange={setIsOpen} open={isOpen}>
      <ChainOfThoughtHeader>Activity</ChainOfThoughtHeader>
      <ChainOfThoughtContent>
        {parts.map(({ key, part }) => (
          <ActivityPartView key={key} part={part} />
        ))}
      </ChainOfThoughtContent>
    </ChainOfThought>
  );
}

function createMessageRenderItems(parts: UIMessagePart[]) {
  // Parts are positionally stable (appended and mutated in place, never
  // reordered), so a per-type ordinal is a stable key without leaning on the
  // raw array index.
  const seen: Record<string, number> = {};
  const renderItems: RenderItem[] = [];
  let pendingActivity:
    | { keys: string[]; parts: KeyedActivityPart[] }
    | undefined;

  const flushActivity = () => {
    if (!pendingActivity) {
      return;
    }

    renderItems.push({
      hasFollowingText: false,
      key: `activity-${pendingActivity.keys.join("-")}`,
      parts: pendingActivity.parts,
      type: "activity",
    });
    pendingActivity = undefined;
  };

  for (const part of parts) {
    seen[part.type] = (seen[part.type] ?? 0) + 1;
    const key =
      part.type === "dynamic-tool"
        ? part.toolCallId
        : `${part.type}-${seen[part.type]}`;

    if (part.type === "text") {
      flushActivity();
      if (part.text) {
        renderItems.push({ key, part, type: "text" });
      }
      continue;
    }

    if (isActivityPart(part)) {
      if (part.type === "reasoning" && !part.text.trim()) {
        continue;
      }

      pendingActivity ??= { keys: [], parts: [] };
      pendingActivity.keys.push(key);
      pendingActivity.parts.push({ key, part });
      continue;
    }

    // `file` and `data-*` parts have no surface in this slice.
    flushActivity();
  }

  flushActivity();

  return renderItems.map((item, index) =>
    item.type === "activity"
      ? {
          ...item,
          hasFollowingText: renderItems
            .slice(index + 1)
            .some((nextItem) => nextItem.type === "text"),
        }
      : item,
  );
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
        <ActivityView
          hasFollowingText={item.hasFollowingText}
          isLatestMessage={isLatestMessage}
          parts={item.parts}
        />
      );
  }
}
