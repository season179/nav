import type { UIMessage, UIMessagePart } from "@flue/react";

import { MessageResponse } from "@/components/ai-elements/message";
import {
  Reasoning,
  ReasoningContent,
  ReasoningTrigger,
} from "@/components/ai-elements/reasoning";
import {
  Tool,
  ToolContent,
  ToolHeader,
  ToolInput,
  ToolOutput,
} from "@/components/ai-elements/tool";

// Renders a Flue UIMessage by walking its parts in order instead of flattening
// everything into assistant text: `text` becomes the markdown answer,
// `reasoning` gets its own collapsible, and tool calls render as their own
// surface. Narrow by `part.type` — these are @flue/react's part types, which
// only overlap with the AI SDK types the generated components are written
// against.
function MessagePartView({ part }: { part: UIMessagePart }) {
  switch (part.type) {
    case "text":
      return part.text ? <MessageResponse>{part.text}</MessageResponse> : null;

    case "reasoning":
      // Don't render empty reasoning chrome (e.g. a started-but-empty block).
      return part.text.trim() ? (
        <Reasoning isStreaming={part.state === "streaming"}>
          <ReasoningTrigger />
          <ReasoningContent>{part.text}</ReasoningContent>
        </Reasoning>
      ) : null;

    case "dynamic-tool": {
      const output =
        part.state === "output-available" ? part.output : undefined;
      const errorText =
        part.state === "output-error" ? part.errorText : undefined;

      return (
        <Tool defaultOpen={part.state !== "output-available"}>
          <ToolHeader
            state={part.state}
            toolName={part.toolName}
            type={part.type}
          />
          <ToolContent>
            <ToolInput input={part.input} />
            <ToolOutput errorText={errorText} output={output} />
          </ToolContent>
        </Tool>
      );
    }

    default:
      // `file` and `data-*` parts have no surface in this slice.
      return null;
  }
}

export function ChatMessageParts({ message }: { message: UIMessage }) {
  // Parts are positionally stable (appended and mutated in place, never
  // reordered), so a per-type ordinal is a stable key without leaning on the
  // raw array index.
  const seen: Record<string, number> = {};
  const keyed = message.parts.map((part) => {
    seen[part.type] = (seen[part.type] ?? 0) + 1;
    const key =
      part.type === "dynamic-tool"
        ? part.toolCallId
        : `${part.type}-${seen[part.type]}`;

    return { key, part };
  });

  return (
    <>
      {keyed.map(({ key, part }) => (
        <MessagePartView key={key} part={part} />
      ))}
    </>
  );
}
