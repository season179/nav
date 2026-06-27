import type { UIMessage, UIMessagePart } from "@flue/react";
import { WrenchIcon } from "lucide-react";
import { useEffect, useRef, useState } from "react";

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
// thinking/tool parts share one stable Activity block. Narrow by `part.type`
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

  return (
    <ChainOfThought onOpenChange={handleOpenChange} open={isOpen}>
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
