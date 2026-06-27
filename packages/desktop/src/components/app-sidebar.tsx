import {
  MoreHorizontalIcon,
  PencilIcon,
  PlusIcon,
  Trash2Icon,
} from "lucide-react";
import { type FormEvent, useEffect, useRef, useState } from "react";

import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import {
  Sidebar,
  SidebarContent,
  SidebarGroup,
  SidebarGroupContent,
  SidebarGroupLabel,
  SidebarHeader,
  SidebarMenu,
  SidebarMenuAction,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarMenuSkeleton,
  SidebarRail,
} from "@/components/ui/sidebar";
import type { NavSession } from "@/lib/sessions-client";

type AppSidebarProps = {
  activeSessionId: string | null;
  error: string | null;
  isDraftActive: boolean;
  loading: boolean;
  onDeleteSession: (id: string) => Promise<void>;
  onNewChat: () => void;
  onRenameSession: (id: string, title: string) => Promise<void>;
  onSelectSession: (id: string) => void;
  sessions: NavSession[];
};

const displayTitle = (session: NavSession) => session.title ?? "Untitled chat";
const skeletonKeys = [
  "session-skeleton-a",
  "session-skeleton-b",
  "session-skeleton-c",
  "session-skeleton-d",
  "session-skeleton-e",
  "session-skeleton-f",
];

export function AppSidebar({
  activeSessionId,
  error,
  isDraftActive,
  loading,
  onDeleteSession,
  onNewChat,
  onRenameSession,
  onSelectSession,
  sessions,
}: AppSidebarProps) {
  const [actionError, setActionError] = useState<string | null>(null);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editingTitle, setEditingTitle] = useState("");
  const [pendingId, setPendingId] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    if (editingId) {
      inputRef.current?.focus();
      inputRef.current?.select();
    }
  }, [editingId]);

  const runAction = async (id: string, action: () => Promise<void>) => {
    setActionError(null);
    setPendingId(id);

    try {
      await action();
    } catch (caught) {
      setActionError(
        caught instanceof Error ? caught.message : "Unable to update chat.",
      );
    } finally {
      setPendingId(null);
    }
  };

  const startEditing = (session: NavSession) => {
    setActionError(null);
    setEditingId(session.id);
    setEditingTitle(displayTitle(session));
  };

  const cancelEditing = () => {
    setEditingId(null);
    setEditingTitle("");
  };

  const submitRename = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();

    if (!editingId) {
      return;
    }

    const title = editingTitle.trim();

    if (!title) {
      return;
    }

    await runAction(editingId, async () => {
      await onRenameSession(editingId, title);
      cancelEditing();
    });
  };

  const message = error ?? actionError;

  return (
    <Sidebar>
      <SidebarHeader className="h-10" />
      <SidebarContent>
        <SidebarGroup>
          <SidebarGroupContent>
            <SidebarMenu>
              <SidebarMenuItem>
                <SidebarMenuButton
                  isActive={isDraftActive}
                  onClick={onNewChat}
                  type="button"
                >
                  <PlusIcon aria-hidden="true" />
                  <span>New chat</span>
                </SidebarMenuButton>
              </SidebarMenuItem>
            </SidebarMenu>
          </SidebarGroupContent>
        </SidebarGroup>
        <SidebarGroup>
          <SidebarGroupLabel>Chats</SidebarGroupLabel>
          <SidebarGroupContent>
            {message && (
              <p className="px-2 pb-2 text-destructive text-xs">{message}</p>
            )}
            <SidebarMenu>
              {loading && sessions.length === 0
                ? skeletonKeys.map((key) => (
                    <SidebarMenuItem key={key}>
                      <SidebarMenuSkeleton />
                    </SidebarMenuItem>
                  ))
                : sessions.map((session) => {
                    const title = displayTitle(session);
                    const isActive = activeSessionId === session.id;
                    const isEditing = editingId === session.id;
                    const isPending = pendingId === session.id;

                    return (
                      <SidebarMenuItem key={session.id}>
                        {isEditing ? (
                          <form className="px-1 py-0.5" onSubmit={submitRename}>
                            <Input
                              aria-label="Chat title"
                              className="h-7 bg-background"
                              disabled={isPending}
                              onBlur={() => {
                                if (!isPending) {
                                  cancelEditing();
                                }
                              }}
                              onChange={(event) =>
                                setEditingTitle(event.target.value)
                              }
                              onKeyDown={(event) => {
                                if (event.key === "Escape") {
                                  event.preventDefault();
                                  cancelEditing();
                                }
                              }}
                              ref={inputRef}
                              value={editingTitle}
                            />
                          </form>
                        ) : (
                          <>
                            <SidebarMenuButton
                              aria-current={isActive ? "page" : undefined}
                              disabled={isPending}
                              isActive={isActive}
                              onClick={() => onSelectSession(session.id)}
                              title={
                                session.lastPreview
                                  ? `${title} - ${session.lastPreview}`
                                  : title
                              }
                              type="button"
                            >
                              <span>{title}</span>
                            </SidebarMenuButton>
                            <DropdownMenu>
                              <DropdownMenuTrigger asChild>
                                <SidebarMenuAction
                                  aria-label="Chat actions"
                                  disabled={isPending}
                                  showOnHover
                                  type="button"
                                >
                                  <MoreHorizontalIcon aria-hidden="true" />
                                </SidebarMenuAction>
                              </DropdownMenuTrigger>
                              <DropdownMenuContent align="end" className="w-36">
                                <DropdownMenuItem
                                  onSelect={(event) => {
                                    event.preventDefault();
                                    startEditing(session);
                                  }}
                                >
                                  <PencilIcon aria-hidden="true" />
                                  Rename
                                </DropdownMenuItem>
                                <DropdownMenuItem
                                  onSelect={(event) => {
                                    event.preventDefault();

                                    if (!window.confirm(`Delete "${title}"?`)) {
                                      return;
                                    }

                                    void runAction(session.id, () =>
                                      onDeleteSession(session.id),
                                    );
                                  }}
                                  variant="destructive"
                                >
                                  <Trash2Icon aria-hidden="true" />
                                  Delete
                                </DropdownMenuItem>
                              </DropdownMenuContent>
                            </DropdownMenu>
                          </>
                        )}
                      </SidebarMenuItem>
                    );
                  })}
              {!loading && sessions.length === 0 && (
                <SidebarMenuItem>
                  <div className="px-2 py-1.5 text-muted-foreground text-xs">
                    No chats yet
                  </div>
                </SidebarMenuItem>
              )}
            </SidebarMenu>
          </SidebarGroupContent>
        </SidebarGroup>
      </SidebarContent>
      <SidebarRail />
    </Sidebar>
  );
}
