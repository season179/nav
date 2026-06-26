import type { UIMessage } from "ai";
import { StrictMode } from "react";
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
import { AppSidebar } from "@/components/app-sidebar";
import {
  SidebarInset,
  SidebarProvider,
  SidebarTrigger,
} from "@/components/ui/sidebar";

import "./styles.css";

const starterMessages = [
  {
    id: "starter-user",
    parts: [
      {
        text: "What should this space become?",
        type: "text",
      },
    ],
    role: "user",
  },
  {
    id: "starter-assistant",
    parts: [
      {
        text: "A live Nav conversation surface. The desktop shell is ready for the Flue-backed chat flow when that integration lands.",
        type: "text",
      },
    ],
    role: "assistant",
  },
] satisfies UIMessage[];

const getMessageText = (message: UIMessage) =>
  message.parts
    .filter((part) => part.type === "text")
    .map((part) => part.text)
    .join("");

function App() {
  return (
    <SidebarProvider>
      <AppSidebar />
      <div className="fixed inset-x-0 top-0 z-40 h-10 [-webkit-app-region:drag]" />
      <SidebarTrigger className="fixed top-1 left-[76px] z-50 [-webkit-app-region:no-drag] [&_svg]:!size-[18px]" />
      <SidebarInset className="min-h-svh overflow-hidden pt-10">
        <Conversation className="min-h-0">
          <ConversationContent className="mx-auto w-full max-w-3xl px-6 pt-14 pb-8">
            {starterMessages.map((message) => (
              <Message from={message.role} key={message.id}>
                <MessageContent>
                  <MessageResponse>{getMessageText(message)}</MessageResponse>
                </MessageContent>
              </Message>
            ))}
          </ConversationContent>
          <ConversationScrollButton aria-label="Scroll to bottom" />
        </Conversation>
      </SidebarInset>
    </SidebarProvider>
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
