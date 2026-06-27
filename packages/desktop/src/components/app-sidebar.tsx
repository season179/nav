import {
  ChevronDownIcon,
  ChevronRightIcon,
  CircleAlertIcon,
  FolderIcon,
  FolderPlusIcon,
  MoreHorizontalIcon,
  PencilIcon,
  PlusIcon,
  Trash2Icon,
} from "lucide-react";
import { type FormEvent, Fragment, useEffect, useRef, useState } from "react";

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
  SidebarGroupAction,
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
import type { NavProject } from "@/lib/projects-client";
import type { NavSession } from "@/lib/sessions-client";

type AppSidebarProps = {
  activeProjectId: string | null;
  activeSessionId: string | null;
  error: string | null;
  loading: boolean;
  onAddProject: () => Promise<void>;
  onDeleteSession: (id: string) => Promise<void>;
  onNewChat: (projectId: string) => void;
  onRemoveProject: (id: string) => Promise<void>;
  onRenameProject: (id: string, name: string) => Promise<void>;
  onRenameSession: (id: string, title: string) => Promise<void>;
  onSelectProject: (id: string) => void;
  onSelectSession: (session: NavSession) => void;
  projects: NavProject[];
  sessions: NavSession[];
};

type EditingTarget = { id: string; kind: "project" | "session" };

const COLLAPSED_PROJECTS_STORAGE_KEY = "nav.collapsedProjectIds";
const MAX_VISIBLE_SESSIONS = 5;
const displayTitle = (session: NavSession) => session.title ?? "Untitled chat";
const skeletonKeys = [
  "project-skeleton-a",
  "project-skeleton-b",
  "project-skeleton-c",
  "project-skeleton-d",
  "project-skeleton-e",
];

const getRememberedCollapsedProjects = (): Record<string, boolean> => {
  try {
    const parsed: unknown = JSON.parse(
      window.localStorage.getItem(COLLAPSED_PROJECTS_STORAGE_KEY) ?? "[]",
    );

    if (!Array.isArray(parsed)) {
      return {};
    }

    return Object.fromEntries(
      parsed
        .filter((value): value is string => typeof value === "string")
        .map((id) => [id, true]),
    );
  } catch {
    return {};
  }
};

const rememberCollapsedProjects = (value: Record<string, boolean>) => {
  window.localStorage.setItem(
    COLLAPSED_PROJECTS_STORAGE_KEY,
    JSON.stringify(Object.keys(value).filter((id) => value[id])),
  );
};

const formatRelativeTime = (timestamp: number) => {
  const seconds = Math.max(0, Math.floor((Date.now() - timestamp) / 1000));

  if (seconds < 60) {
    return "now";
  }

  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) {
    return `${minutes}m`;
  }

  const hours = Math.floor(minutes / 60);
  if (hours < 24) {
    return `${hours}h`;
  }

  const days = Math.floor(hours / 24);
  if (days < 7) {
    return `${days}d`;
  }

  const weeks = Math.floor(days / 7);
  if (weeks < 8) {
    return `${weeks}w`;
  }

  const months = Math.floor(days / 30);
  if (months < 12) {
    return `${months}mo`;
  }

  return `${Math.floor(days / 365)}y`;
};

export function AppSidebar({
  activeProjectId,
  activeSessionId,
  error,
  loading,
  onAddProject,
  onDeleteSession,
  onNewChat,
  onRemoveProject,
  onRenameProject,
  onRenameSession,
  onSelectProject,
  onSelectSession,
  projects,
  sessions,
}: AppSidebarProps) {
  const [actionError, setActionError] = useState<string | null>(null);
  const [collapsedProjectIds, setCollapsedProjectIds] = useState<
    Record<string, boolean>
  >(() => getRememberedCollapsedProjects());
  const [editing, setEditing] = useState<EditingTarget | null>(null);
  const [editingTitle, setEditingTitle] = useState("");
  const [pendingId, setPendingId] = useState<string | null>(null);
  const [showAllProjectIds, setShowAllProjectIds] = useState<
    Record<string, boolean>
  >({});
  const inputRef = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    if (editing) {
      inputRef.current?.focus();
      inputRef.current?.select();
    }
  }, [editing]);

  const runAction = async (id: string, action: () => Promise<void>) => {
    setActionError(null);
    setPendingId(id);

    try {
      await action();
    } catch (caught) {
      setActionError(
        caught instanceof Error ? caught.message : "Unable to update Nav.",
      );
    } finally {
      setPendingId(null);
    }
  };

  const toggleProject = (id: string) => {
    setCollapsedProjectIds((current) => {
      const next = { ...current };

      if (next[id]) {
        delete next[id];
      } else {
        next[id] = true;
      }

      rememberCollapsedProjects(next);
      return next;
    });
  };

  const toggleShowAll = (id: string) => {
    setShowAllProjectIds((current) => ({
      ...current,
      [id]: !current[id],
    }));
  };

  const startEditingProject = (project: NavProject) => {
    setActionError(null);
    setEditing({ id: project.id, kind: "project" });
    setEditingTitle(project.name);
  };

  const startEditingSession = (session: NavSession) => {
    setActionError(null);
    setEditing({ id: session.id, kind: "session" });
    setEditingTitle(displayTitle(session));
  };

  const cancelEditing = () => {
    setEditing(null);
    setEditingTitle("");
  };

  const submitRename = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();

    if (!editing) {
      return;
    }

    const title = editingTitle.trim();

    if (!title) {
      return;
    }

    await runAction(editing.id, async () => {
      if (editing.kind === "project") {
        await onRenameProject(editing.id, title);
      } else {
        await onRenameSession(editing.id, title);
      }

      cancelEditing();
    });
  };

  const message = error ?? actionError;

  return (
    <Sidebar>
      <SidebarHeader className="h-10" />
      <SidebarContent>
        <SidebarGroup>
          <SidebarGroupLabel>Projects</SidebarGroupLabel>
          <SidebarGroupAction
            aria-label="Add project"
            disabled={pendingId === "add-project"}
            onClick={() => void runAction("add-project", onAddProject)}
            type="button"
          >
            <FolderPlusIcon aria-hidden="true" />
          </SidebarGroupAction>
          <SidebarGroupContent>
            {message && (
              <p className="px-2 pb-2 text-destructive text-xs">{message}</p>
            )}
            <SidebarMenu>
              {loading && projects.length === 0
                ? skeletonKeys.map((key) => (
                    <SidebarMenuItem key={key}>
                      <SidebarMenuSkeleton showIcon />
                    </SidebarMenuItem>
                  ))
                : projects.map((project) => {
                    const isActiveProject = activeProjectId === project.id;
                    const isCollapsed =
                      collapsedProjectIds[project.id] === true;
                    const projectSessions = sessions.filter(
                      (session) => session.projectId === project.id,
                    );
                    const showAll = showAllProjectIds[project.id] === true;
                    const visibleSessions = showAll
                      ? projectSessions
                      : projectSessions.slice(0, MAX_VISIBLE_SESSIONS);
                    const hiddenSessionCount =
                      projectSessions.length - visibleSessions.length;
                    const isProjectPending = pendingId === project.id;
                    const isEditingProject =
                      editing?.kind === "project" && editing.id === project.id;
                    const projectPath = project.displayPath ?? project.path;

                    return (
                      <Fragment key={project.id}>
                        <SidebarMenuItem>
                          {isEditingProject ? (
                            <form
                              className="px-1 py-0.5"
                              onSubmit={submitRename}
                            >
                              <Input
                                aria-label="Project name"
                                className="h-7 bg-background"
                                disabled={isProjectPending}
                                onBlur={() => {
                                  if (!isProjectPending) {
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
                              <button
                                aria-label={
                                  isCollapsed
                                    ? `Expand ${project.name}`
                                    : `Collapse ${project.name}`
                                }
                                className="absolute top-1.5 left-1 z-10 flex size-5 items-center justify-center rounded-md text-sidebar-foreground/70 hover:bg-sidebar-accent hover:text-sidebar-accent-foreground"
                                onClick={() => toggleProject(project.id)}
                                type="button"
                              >
                                {isCollapsed ? (
                                  <ChevronRightIcon
                                    aria-hidden="true"
                                    className="size-3.5"
                                  />
                                ) : (
                                  <ChevronDownIcon
                                    aria-hidden="true"
                                    className="size-3.5"
                                  />
                                )}
                              </button>
                              <SidebarMenuButton
                                aria-current={
                                  isActiveProject ? "page" : undefined
                                }
                                className="pl-7 pr-14"
                                disabled={isProjectPending}
                                isActive={isActiveProject}
                                onClick={() => onSelectProject(project.id)}
                                title={`${project.name} - ${projectPath}`}
                                type="button"
                              >
                                <FolderIcon aria-hidden="true" />
                                <span className="min-w-0 flex-1 truncate">
                                  {project.name}
                                </span>
                                {!project.available && (
                                  <CircleAlertIcon
                                    aria-hidden="true"
                                    className="text-destructive"
                                  />
                                )}
                              </SidebarMenuButton>
                              {isActiveProject && (
                                <SidebarMenuAction
                                  aria-label="New chat"
                                  className="right-7"
                                  disabled={
                                    isProjectPending || !project.available
                                  }
                                  onClick={() => onNewChat(project.id)}
                                  showOnHover
                                  type="button"
                                >
                                  <PlusIcon aria-hidden="true" />
                                </SidebarMenuAction>
                              )}
                              {isActiveProject && (
                                <DropdownMenu>
                                  <DropdownMenuTrigger asChild>
                                    <SidebarMenuAction
                                      aria-label="Project actions"
                                      disabled={isProjectPending}
                                      showOnHover
                                      type="button"
                                    >
                                      <MoreHorizontalIcon aria-hidden="true" />
                                    </SidebarMenuAction>
                                  </DropdownMenuTrigger>
                                  <DropdownMenuContent
                                    align="end"
                                    className="w-40"
                                  >
                                    <DropdownMenuItem
                                      onSelect={(event) => {
                                        event.preventDefault();
                                        startEditingProject(project);
                                      }}
                                    >
                                      <PencilIcon aria-hidden="true" />
                                      Rename
                                    </DropdownMenuItem>
                                    {!project.isDefault && (
                                      <DropdownMenuItem
                                        onSelect={(event) => {
                                          event.preventDefault();

                                          if (
                                            !window.confirm(
                                              `Remove "${project.name}"?`,
                                            )
                                          ) {
                                            return;
                                          }

                                          void runAction(project.id, () =>
                                            onRemoveProject(project.id),
                                          );
                                        }}
                                        variant="destructive"
                                      >
                                        <Trash2Icon aria-hidden="true" />
                                        Remove
                                      </DropdownMenuItem>
                                    )}
                                  </DropdownMenuContent>
                                </DropdownMenu>
                              )}
                            </>
                          )}
                        </SidebarMenuItem>
                        {!isCollapsed &&
                          visibleSessions.map((session) => {
                            const title = displayTitle(session);
                            const isActive = activeSessionId === session.id;
                            const isEditingSession =
                              editing?.kind === "session" &&
                              editing.id === session.id;
                            const isSessionPending = pendingId === session.id;

                            return (
                              <SidebarMenuItem key={session.id}>
                                {isEditingSession ? (
                                  <form
                                    className="py-0.5 pr-1 pl-7"
                                    onSubmit={submitRename}
                                  >
                                    <Input
                                      aria-label="Chat title"
                                      className="h-7 bg-background"
                                      disabled={isSessionPending}
                                      onBlur={() => {
                                        if (!isSessionPending) {
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
                                      aria-current={
                                        isActive ? "page" : undefined
                                      }
                                      className="h-7 pl-8 text-xs"
                                      disabled={isSessionPending}
                                      isActive={isActive}
                                      onClick={() => onSelectSession(session)}
                                      title={
                                        session.lastPreview
                                          ? `${title} - ${session.lastPreview}`
                                          : title
                                      }
                                      type="button"
                                    >
                                      <span className="min-w-0 flex-1 truncate">
                                        {title}
                                      </span>
                                      <span className="shrink-0 text-[10px] text-muted-foreground tabular-nums">
                                        {formatRelativeTime(session.updatedAt)}
                                      </span>
                                    </SidebarMenuButton>
                                    <DropdownMenu>
                                      <DropdownMenuTrigger asChild>
                                        <SidebarMenuAction
                                          aria-label="Chat actions"
                                          disabled={isSessionPending}
                                          showOnHover
                                          type="button"
                                        >
                                          <MoreHorizontalIcon aria-hidden="true" />
                                        </SidebarMenuAction>
                                      </DropdownMenuTrigger>
                                      <DropdownMenuContent
                                        align="end"
                                        className="w-36"
                                      >
                                        <DropdownMenuItem
                                          onSelect={(event) => {
                                            event.preventDefault();
                                            startEditingSession(session);
                                          }}
                                        >
                                          <PencilIcon aria-hidden="true" />
                                          Rename
                                        </DropdownMenuItem>
                                        <DropdownMenuItem
                                          onSelect={(event) => {
                                            event.preventDefault();

                                            if (
                                              !window.confirm(
                                                `Delete "${title}"?`,
                                              )
                                            ) {
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
                        {!isCollapsed && hiddenSessionCount > 0 && (
                          <SidebarMenuItem key={`${project.id}-show-more`}>
                            <SidebarMenuButton
                              className="h-7 pl-8 text-muted-foreground text-xs"
                              onClick={() => toggleShowAll(project.id)}
                              type="button"
                            >
                              <span>
                                {showAll
                                  ? "Show less"
                                  : `Show more (${hiddenSessionCount})`}
                              </span>
                            </SidebarMenuButton>
                          </SidebarMenuItem>
                        )}
                        {!isCollapsed && projectSessions.length === 0 && (
                          <SidebarMenuItem key={`${project.id}-empty`}>
                            <div className="px-8 py-1.5 text-muted-foreground text-xs">
                              No chats yet
                            </div>
                          </SidebarMenuItem>
                        )}
                      </Fragment>
                    );
                  })}
              {!loading && projects.length === 0 && (
                <SidebarMenuItem>
                  <div className="px-2 py-1.5 text-muted-foreground text-xs">
                    No projects yet
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
