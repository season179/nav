import { useChat } from "@ai-sdk/react";
import type { UIMessage } from "ai";
import { DefaultChatTransport } from "ai";
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
  PromptInputActionAddAttachments,
  PromptInputActionAddScreenshot,
  PromptInputActionMenu,
  PromptInputActionMenuContent,
  PromptInputActionMenuTrigger,
  PromptInputBody,
  PromptInputFooter,
  PromptInputSubmit,
  PromptInputTextarea,
  PromptInputTools,
} from "@/components/ai-elements/prompt-input";
import { AppSidebar } from "@/components/app-sidebar";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import {
  SidebarInset,
  SidebarProvider,
  SidebarTrigger,
} from "@/components/ui/sidebar";
import { TooltipProvider } from "@/components/ui/tooltip";

import "./styles.css";

const agentServerUrl =
  import.meta.env.VITE_NAV_AGENT_SERVER_URL ?? "http://127.0.0.1:3583";

const getMessageText = (message: UIMessage) =>
  message.parts
    .filter((part) => part.type === "text")
    .map((part) => part.text)
    .join("");

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
            <MessageContent>
              <MessageResponse>{getMessageText(message)}</MessageResponse>
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
        <PromptInputBody>
          <PromptInputTextarea placeholder="Message Nav" />
        </PromptInputBody>
        <PromptInputFooter>
          <PromptInputTools>
            <PromptInputActionMenu>
              <PromptInputActionMenuTrigger
                aria-label="Add context"
                tooltip="Add context"
              />
              <PromptInputActionMenuContent>
                <PromptInputActionAddAttachments />
                <PromptInputActionAddScreenshot />
              </PromptInputActionMenuContent>
            </PromptInputActionMenu>
          </PromptInputTools>
          <PromptInputSubmit onStop={stop} status={status} />
        </PromptInputFooter>
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
