import { useChat } from "@ai-sdk/react";
import {
  DefaultChatTransport,
  type DynamicToolUIPart,
  isDynamicToolUIPart,
  isToolUIPart,
  type ToolUIPart,
  type UIMessage,
} from "ai";
import { MessagesSquareIcon } from "lucide-react";
import { StrictMode, useMemo } from "react";
import { createRoot } from "react-dom/client";

import {
  Conversation,
  ConversationContent,
  ConversationScrollButton,
} from "@/components/ai-elements/conversation";
import {
  Message,
  MessageContent,
  MessageResponse,
} from "@/components/ai-elements/message";
import {
  PromptInput,
  PromptInputSubmit,
  PromptInputTextarea,
} from "@/components/ai-elements/prompt-input";
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
import { AppSidebar } from "@/components/app-sidebar";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import { InputGroupAddon } from "@/components/ui/input-group";
import {
  SidebarInset,
  SidebarProvider,
  SidebarTrigger,
} from "@/components/ui/sidebar";
import { TooltipProvider } from "@/components/ui/tooltip";

import "./styles.css";

const agentServerUrl =
  import.meta.env.VITE_NAV_AGENT_SERVER_URL ?? "http://127.0.0.1:3583";

type ToolMessagePart = DynamicToolUIPart | ToolUIPart;

function ChatToolPart({ part }: { part: ToolMessagePart }) {
  return (
    <Tool defaultOpen={part.state !== "output-available"}>
      {isDynamicToolUIPart(part) ? (
        <ToolHeader
          state={part.state}
          toolName={part.toolName}
          type={part.type}
        />
      ) : (
        <ToolHeader state={part.state} type={part.type} />
      )}
      <ToolContent>
        {part.input === undefined ? null : <ToolInput input={part.input} />}
        <ToolOutput errorText={part.errorText} output={part.output} />
      </ToolContent>
    </Tool>
  );
}

function ChatMessageParts({ message }: { message: UIMessage }) {
  return message.parts.map((part, index) => {
    const key = `${message.id}-${part.type}-${index}`;

    if (part.type === "text") {
      return <MessageResponse key={key}>{part.text}</MessageResponse>;
    }

    if (part.type === "reasoning") {
      return (
        <Reasoning
          defaultOpen={part.state === "streaming"}
          isStreaming={part.state === "streaming"}
          key={key}
        >
          <ReasoningTrigger />
          <ReasoningContent>{part.text}</ReasoningContent>
        </Reasoning>
      );
    }

    if (isToolUIPart(part) || isDynamicToolUIPart(part)) {
      return <ChatToolPart key={key} part={part} />;
    }

    return null;
  });
}

function EmptyConversation() {
  return (
    <Empty className="min-h-0 border-0 px-6 py-10">
      <EmptyHeader>
        <EmptyMedia className="size-10 rounded-xl" variant="icon">
          <MessagesSquareIcon aria-hidden="true" className="size-5" />
        </EmptyMedia>
        <EmptyTitle>Nav</EmptyTitle>
        <EmptyDescription>Ask about this workspace.</EmptyDescription>
      </EmptyHeader>
    </Empty>
  );
}

function ChatConversation({
  error,
  messages,
}: {
  error?: Error;
  messages: UIMessage[];
}) {
  return (
    <Conversation className="min-h-0">
      <ConversationContent className="mx-auto w-full max-w-3xl px-6 pt-14 pb-8">
        {messages.length === 0 ? <EmptyConversation /> : null}
        {messages.map((message) => (
          <Message from={message.role} key={message.id}>
            <MessageContent
              className={message.role === "assistant" ? "w-full" : undefined}
            >
              <ChatMessageParts message={message} />
            </MessageContent>
          </Message>
        ))}
        {error ? (
          <Message from="assistant">
            <MessageContent>
              <MessageResponse>{`Nav hit an error: ${error.message}`}</MessageResponse>
            </MessageContent>
          </Message>
        ) : null}
      </ConversationContent>
      <ConversationScrollButton aria-label="Scroll to bottom" />
    </Conversation>
  );
}

function PromptComposer({
  onSubmit,
  status,
  stop,
}: {
  onSubmit: (message: string) => void;
  status: "error" | "ready" | "submitted" | "streaming";
  stop: () => void;
}) {
  return (
    <div className="shrink-0 bg-background/95 px-4 py-3 backdrop-blur">
      <PromptInput
        aria-label="Chat prompt"
        className="mx-auto max-w-3xl"
        onSubmit={(message) => onSubmit(message.text)}
      >
        <PromptInputTextarea placeholder="Message Nav" />
        <InputGroupAddon align="inline-end">
          <PromptInputSubmit onStop={stop} status={status} />
        </InputGroupAddon>
      </PromptInput>
    </div>
  );
}

function App() {
  const transport = useMemo(
    () =>
      new DefaultChatTransport({
        api: `${agentServerUrl}/api/chat`,
      }),
    [],
  );
  const { error, messages, sendMessage, status, stop } = useChat({
    transport,
  });

  const handleSubmit = (text: string) => {
    const trimmedText = text.trim();
    if (!trimmedText || status === "submitted" || status === "streaming") {
      return;
    }

    sendMessage({ text: trimmedText });
  };

  return (
    <TooltipProvider>
      <SidebarProvider>
        <AppSidebar />
        <div className="fixed inset-x-0 top-0 z-40 h-10 [-webkit-app-region:drag]" />
        <SidebarTrigger className="fixed top-1 left-[76px] z-50 [-webkit-app-region:no-drag] [&_svg]:!size-[18px]" />
        <SidebarInset className="min-h-svh overflow-hidden pt-10">
          <div className="flex min-h-0 flex-1 flex-col">
            <ChatConversation error={error} messages={messages} />
            <PromptComposer
              onSubmit={handleSubmit}
              status={status}
              stop={stop}
            />
          </div>
        </SidebarInset>
      </SidebarProvider>
    </TooltipProvider>
  );
}

const root = document.createElement("div");
root.id = "root";
document.body.replaceChildren(root);

createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
