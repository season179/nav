import { FlueProvider, type UIMessage, useFlueAgent } from "@flue/react";
import { createFlueClient } from "@flue/sdk";
import {
  CircleAlertIcon,
  LoaderCircleIcon,
  MessageSquareTextIcon,
} from "lucide-react";
import { StrictMode, useEffect, useMemo, useState } from "react";
import { createRoot } from "react-dom/client";

import {
  Conversation,
  ConversationContent,
  ConversationScrollButton,
} from "@/components/ai-elements/conversation";
import { Message, MessageContent } from "@/components/ai-elements/message";
import {
  PromptInput,
  PromptInputBody,
  PromptInputFooter,
  PromptInputSubmit,
  PromptInputTextarea,
  PromptInputTools,
} from "@/components/ai-elements/prompt-input";
import { Shimmer } from "@/components/ai-elements/shimmer";
import { AppSidebar } from "@/components/app-sidebar";
import { ChatMessageParts } from "@/components/chat-message-parts";
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
import type { FlueConnection, FlueServerStatus } from "@/lib/flue-connection";

import "./styles.css";

const formatUuidBytes = (bytes: Uint8Array) =>
  [...bytes]
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("")
    .replace(/^(.{8})(.{4})(.{4})(.{4})(.{12})$/, "$1-$2-$3-$4-$5");

const createUuidV7 = () => {
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);

  const timestamp = BigInt(Date.now());
  bytes[0] = Number((timestamp >> 40n) & 0xffn);
  bytes[1] = Number((timestamp >> 32n) & 0xffn);
  bytes[2] = Number((timestamp >> 24n) & 0xffn);
  bytes[3] = Number((timestamp >> 16n) & 0xffn);
  bytes[4] = Number((timestamp >> 8n) & 0xffn);
  bytes[5] = Number(timestamp & 0xffn);
  bytes[6] = (bytes[6] & 0x0f) | 0x70;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;

  return formatUuidBytes(bytes);
};

function EmptyConversation() {
  return (
    <Empty className="min-h-0 border-0 px-6 py-10">
      <EmptyHeader>
        <EmptyMedia className="size-10 rounded-xl" variant="icon">
          <MessageSquareTextIcon aria-hidden="true" className="size-5" />
        </EmptyMedia>
        <EmptyTitle>Message Nav</EmptyTitle>
        <EmptyDescription>
          Start a conversation with the local Nav agent.
        </EmptyDescription>
      </EmptyHeader>
    </Empty>
  );
}

function ConnectionEmpty({
  message,
  state,
}: {
  message: string;
  state: "failed" | "starting";
}) {
  const Icon = state === "failed" ? CircleAlertIcon : LoaderCircleIcon;

  return (
    <Empty className="min-h-0 border-0 px-6 py-10">
      <EmptyHeader>
        <EmptyMedia className="size-10 rounded-xl" variant="icon">
          <Icon
            aria-hidden="true"
            className={state === "starting" ? "size-5 animate-spin" : "size-5"}
          />
        </EmptyMedia>
        <EmptyTitle>
          {state === "failed" ? "Nav is unavailable" : "Starting Nav"}
        </EmptyTitle>
        <EmptyDescription>{message}</EmptyDescription>
      </EmptyHeader>
    </Empty>
  );
}

type MessagePart = UIMessage["parts"][number];

const hasVisiblePart = (part: MessagePart) => {
  switch (part.type) {
    case "text":
    case "reasoning":
      return part.text.trim().length > 0;
    case "dynamic-tool":
      return true;
    default:
      return false;
  }
};

const hasVisibleAssistantOutputAfterLastUser = (messages: UIMessage[]) => {
  const lastUserIndex = messages.findLastIndex(
    (message) => message.role === "user",
  );

  return messages
    .slice(lastUserIndex + 1)
    .some(
      (message) =>
        message.role === "assistant" && message.parts.some(hasVisiblePart),
    );
};

function ThinkingMessage() {
  return (
    <Message from="assistant">
      <MessageContent>
        <Shimmer duration={1}>Thinking...</Shimmer>
      </MessageContent>
    </Message>
  );
}

function LiveConversation({
  isThinking,
  messages,
}: {
  isThinking: boolean;
  messages: UIMessage[];
}) {
  if (messages.length === 0 && !isThinking) {
    return <EmptyConversation />;
  }

  return (
    <Conversation className="min-h-0">
      <ConversationContent className="mx-auto w-full max-w-3xl px-6 pt-14 pb-8">
        {messages.map((message) => (
          <Message
            from={message.role === "assistant" ? "assistant" : "user"}
            key={message.id}
          >
            <MessageContent>
              <ChatMessageParts message={message} />
            </MessageContent>
          </Message>
        ))}
        {isThinking && <ThinkingMessage />}
      </ConversationContent>
      <ConversationScrollButton aria-label="Scroll to bottom" />
    </Conversation>
  );
}

function PromptComposer({
  disabled,
  onSubmit,
  status,
}: {
  disabled?: boolean;
  onSubmit: (message: string) => Promise<void>;
  status?: "error" | "submitted" | "streaming";
}) {
  return (
    <div className="shrink-0 bg-background/95 px-4 py-3 backdrop-blur">
      <PromptInput
        aria-label="Chat prompt"
        className="mx-auto max-w-3xl"
        onSubmit={async (message) => {
          const text = message.text.trim();

          if (!text || disabled) {
            return;
          }

          await onSubmit(text);
        }}
      >
        <PromptInputBody>
          <PromptInputTextarea disabled={disabled} placeholder="Message Nav" />
        </PromptInputBody>
        <PromptInputFooter>
          <PromptInputTools />
          <PromptInputSubmit disabled={disabled} status={status} />
        </PromptInputFooter>
      </PromptInput>
    </div>
  );
}

function NavChat({ serverStatus }: { serverStatus: FlueServerStatus | null }) {
  const [conversationId] = useState(() => createUuidV7());
  const { messages, status, error, sendMessage } = useFlueAgent({
    id: conversationId,
    name: "nav",
  });

  const serverReady = serverStatus?.state === "ready";
  // Only block the composer while the server is down or a send is in flight.
  // History hydration uses a "connecting" status that retries indefinitely on
  // recoverable backend errors (e.g. a 503 while Codex auth is unavailable);
  // gating on it would leave the composer permanently disabled with no way to
  // type, retry, or surface the error.
  const sending = status === "submitted" || status === "streaming";
  const disabled = !serverReady || sending;

  const composerStatus =
    status === "error"
      ? "error"
      : status === "streaming"
        ? "streaming"
        : status === "submitted"
          ? "submitted"
          : undefined;
  const isThinking =
    status === "submitted" ||
    (status === "streaming" &&
      !hasVisibleAssistantOutputAfterLastUser(messages));

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {error && (
        <div className="mx-auto mt-4 w-full max-w-3xl rounded-md border border-destructive/30 bg-destructive/10 px-3 py-2 text-destructive text-sm">
          {error.message}
        </div>
      )}
      <LiveConversation isThinking={isThinking} messages={messages} />
      <PromptComposer
        disabled={disabled}
        onSubmit={async (text) => {
          await sendMessage(text);
        }}
        status={composerStatus}
      />
    </div>
  );
}

function ConnectedApp({
  connection,
  serverStatus,
}: {
  connection: FlueConnection;
  serverStatus: FlueServerStatus | null;
}) {
  const client = useMemo(
    () =>
      createFlueClient({
        baseUrl: connection.baseUrl,
        fetch: window.fetch.bind(window),
        token: connection.token,
      }),
    [connection.baseUrl, connection.token],
  );

  return (
    <FlueProvider client={client}>
      <NavChat serverStatus={serverStatus} />
    </FlueProvider>
  );
}

function AppContent() {
  const [connection, setConnection] = useState<FlueConnection | null>(null);
  const [connectionError, setConnectionError] = useState<string | null>(null);
  const [serverStatus, setServerStatus] = useState<FlueServerStatus | null>(
    null,
  );

  useEffect(() => {
    const unsubscribe = window.navDesktop.onFlueStatus(setServerStatus);

    window.navDesktop
      .getFlueConnection()
      .then((nextConnection) => {
        setConnection(nextConnection);
        setServerStatus(nextConnection.status);
      })
      .catch((error: unknown) => {
        setConnectionError(
          error instanceof Error ? error.message : "Unable to connect to Nav.",
        );
      });

    return unsubscribe;
  }, []);

  if (connectionError) {
    return (
      <div className="flex min-h-0 flex-1 flex-col">
        <ConnectionEmpty message={connectionError} state="failed" />
      </div>
    );
  }

  if (!connection) {
    return (
      <div className="flex min-h-0 flex-1 flex-col">
        <ConnectionEmpty
          message={
            serverStatus?.message ?? "Waiting for the local Flue server."
          }
          state={serverStatus?.state === "failed" ? "failed" : "starting"}
        />
      </div>
    );
  }

  return <ConnectedApp connection={connection} serverStatus={serverStatus} />;
}

function App() {
  return (
    <TooltipProvider>
      <SidebarProvider>
        <AppSidebar />
        <div className="fixed inset-x-0 top-0 z-40 h-10 [-webkit-app-region:drag]" />
        <SidebarTrigger className="fixed top-1 left-[76px] z-50 [-webkit-app-region:no-drag] [&_svg]:!size-[18px]" />
        <SidebarInset className="min-h-svh overflow-hidden pt-10">
          <AppContent />
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
