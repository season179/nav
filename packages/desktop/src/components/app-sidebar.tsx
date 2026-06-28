import {
  ArchiveIcon,
  ArchiveRestoreIcon,
  ArrowDownIcon,
  ArrowUpIcon,
  BookOpenIcon,
  BotIcon,
  ChevronDownIcon,
  ChevronRightIcon,
  CircleAlertIcon,
  Code2Icon,
  FolderIcon,
  FolderPlusIcon,
  FolderSearchIcon,
  GripVerticalIcon,
  type LucideIcon,
  MoreHorizontalIcon,
  PackageIcon,
  PaletteIcon,
  PencilIcon,
  PlusIcon,
  Settings2Icon,
  ShieldCheckIcon,
  SparklesIcon,
  TerminalIcon,
  Trash2Icon,
} from "lucide-react";
import { type FormEvent, Fragment, useEffect, useRef, useState } from "react";

import {
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
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
import type { NavProject, ProjectUpdate } from "@/lib/projects-client";
import type { NavSession } from "@/lib/sessions-client";

type AppSidebarProps = {
  activeProjectId: string | null;
  activeSessionId: string | null;
  error: string | null;
  loading: boolean;
  onAddProject: () => Promise<void>;
  onDeleteSession: (id: string) => Promise<void>;
  onLocateProject: (id: string) => Promise<void>;
  onNewChat: (projectId: string) => void;
  onReorderProjects: (projectIds: string[]) => Promise<void>;
  onRemoveProject: (id: string) => Promise<void>;
  onRenameProject: (id: string, name: string) => Promise<void>;
  onRestoreProject: (id: string) => Promise<void>;
  onRenameSession: (id: string, title: string) => Promise<void>;
  onSelectProject: (id: string) => void;
  onSelectSession: (session: NavSession) => void;
  onShowArchivedProjectsChange: (value: boolean) => void;
  onUpdateProject: (id: string, update: ProjectUpdate) => Promise<void>;
  projects: NavProject[];
  sessions: NavSession[];
  showArchivedProjects: boolean;
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
const modelOptions = [
  { label: "Default gpt-5.5", value: "default" },
  { label: "Codex gpt-5.5", value: "openai-codex/gpt-5.5" },
  { label: "GLM 5.2", value: "zai/glm-5.2" },
  { label: "DeepSeek V4 Pro", value: "deepseek/deepseek-v4-pro" },
  { label: "DeepSeek V4 Flash", value: "deepseek/deepseek-v4-flash" },
] as const;
const colorOptions = [
  { label: "Default", value: "none", color: "transparent" },
  { label: "Slate", value: "slate", color: "#64748b" },
  { label: "Red", value: "red", color: "#ef4444" },
  { label: "Orange", value: "orange", color: "#f97316" },
  { label: "Yellow", value: "yellow", color: "#eab308" },
  { label: "Green", value: "green", color: "#22c55e" },
  { label: "Teal", value: "teal", color: "#14b8a6" },
  { label: "Blue", value: "blue", color: "#3b82f6" },
  { label: "Violet", value: "violet", color: "#8b5cf6" },
  { label: "Pink", value: "pink", color: "#ec4899" },
] as const;
const iconOptions = [
  { Icon: FolderIcon, label: "Folder", value: "folder" },
  { Icon: Code2Icon, label: "Code", value: "code" },
  { Icon: TerminalIcon, label: "Terminal", value: "terminal" },
  { Icon: PackageIcon, label: "Package", value: "package" },
  { Icon: BookOpenIcon, label: "Book", value: "book" },
  { Icon: SparklesIcon, label: "Spark", value: "spark" },
] as const;
const iconByValue = Object.fromEntries(
  iconOptions.map(({ Icon, value }) => [value, Icon]),
) as Record<string, LucideIcon>;
const colorByValue = Object.fromEntries(
  colorOptions.map(({ color, value }) => [value, color]),
) as Record<string, string>;

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
  onLocateProject,
  onNewChat,
  onReorderProjects,
  onRemoveProject,
  onRenameProject,
  onRestoreProject,
  onRenameSession,
  onSelectProject,
  onSelectSession,
  onShowArchivedProjectsChange,
  onUpdateProject,
  projects,
  sessions,
  showArchivedProjects,
}: AppSidebarProps) {
  const [actionError, setActionError] = useState<string | null>(null);
  const [collapsedProjectIds, setCollapsedProjectIds] = useState<
    Record<string, boolean>
  >(() => getRememberedCollapsedProjects());
  const [draggingProjectId, setDraggingProjectId] = useState<string | null>(
    null,
  );
  const [editing, setEditing] = useState<EditingTarget | null>(null);
  const [editingTitle, setEditingTitle] = useState("");
  const [pendingId, setPendingId] = useState<string | null>(null);
  const [showAllProjectIds, setShowAllProjectIds] = useState<
    Record<string, boolean>
  >({});
  const inputRef = useRef<HTMLInputElement | null>(null);
  const activeProjects = projects.filter((project) => !project.archived);
  const archivedProjects = projects.filter((project) => project.archived);

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

  const reorderActiveProjects = async (
    projectId: string,
    nextIndex: number,
  ) => {
    const orderedIds = activeProjects.map((project) => project.id);
    const currentIndex = orderedIds.indexOf(projectId);

    if (currentIndex === -1) {
      return;
    }

    const boundedIndex = Math.max(
      0,
      Math.min(nextIndex, orderedIds.length - 1),
    );

    if (boundedIndex === currentIndex) {
      return;
    }

    const [id] = orderedIds.splice(currentIndex, 1);

    if (!id) {
      return;
    }

    orderedIds.splice(boundedIndex, 0, id);
    await onReorderProjects(orderedIds);
  };

  const dropProjectOn = async (targetId: string) => {
    if (!draggingProjectId || draggingProjectId === targetId) {
      setDraggingProjectId(null);
      return;
    }

    const targetIndex = activeProjects.findIndex(
      (project) => project.id === targetId,
    );

    try {
      await reorderActiveProjects(draggingProjectId, targetIndex);
    } finally {
      setDraggingProjectId(null);
    }
  };

  const updateProject = (id: string, update: ProjectUpdate) =>
    runAction(id, () => onUpdateProject(id, update));

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
                : projects.map((project, index) => {
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
                    const projectIndex = activeProjects.findIndex(
                      (candidate) => candidate.id === project.id,
                    );
                    const ProjectIcon =
                      iconByValue[project.icon ?? "folder"] ?? FolderIcon;
                    const projectColor = project.color
                      ? colorByValue[project.color]
                      : null;
                    const isFirstArchivedProject =
                      project.archived && !projects[index - 1]?.archived;

                    return (
                      <Fragment key={project.id}>
                        {isFirstArchivedProject && (
                          <SidebarMenuItem key="archived-projects-label">
                            <div className="px-2 pt-2 pb-1 text-muted-foreground text-xs">
                              Archived
                            </div>
                          </SidebarMenuItem>
                        )}
                        <SidebarMenuItem
                          draggable={!project.archived && !isEditingProject}
                          onDragEnd={() => setDraggingProjectId(null)}
                          onDragOver={(event) => {
                            if (draggingProjectId && !project.archived) {
                              event.preventDefault();
                              event.dataTransfer.dropEffect = "move";
                            }
                          }}
                          onDragStart={(event) => {
                            if (project.archived || isEditingProject) {
                              return;
                            }

                            setDraggingProjectId(project.id);
                            event.dataTransfer.effectAllowed = "move";
                            event.dataTransfer.setData(
                              "text/plain",
                              project.id,
                            );
                          }}
                          onDrop={(event) => {
                            if (project.archived) {
                              return;
                            }

                            event.preventDefault();
                            void runAction("project-order", () =>
                              dropProjectOn(project.id),
                            );
                          }}
                        >
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
                                className={`pl-7 pr-14 ${
                                  project.archived
                                    ? "opacity-60"
                                    : "cursor-grab active:cursor-grabbing"
                                }`}
                                disabled={isProjectPending || project.archived}
                                isActive={isActiveProject}
                                onClick={() => onSelectProject(project.id)}
                                title={`${project.name} - ${projectPath}`}
                                type="button"
                              >
                                {!project.archived && (
                                  <GripVerticalIcon
                                    aria-hidden="true"
                                    className="-ml-1 size-3 text-muted-foreground/70"
                                  />
                                )}
                                <span
                                  className="flex size-4 shrink-0 items-center justify-center rounded-sm"
                                  style={
                                    projectColor
                                      ? { backgroundColor: projectColor }
                                      : undefined
                                  }
                                >
                                  <ProjectIcon
                                    aria-hidden="true"
                                    className={
                                      projectColor
                                        ? "size-3 text-white"
                                        : "size-4"
                                    }
                                  />
                                </span>
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
                                  className="w-52"
                                >
                                  {project.archived && (
                                    <DropdownMenuItem
                                      onSelect={(event) => {
                                        event.preventDefault();
                                        void runAction(project.id, () =>
                                          onRestoreProject(project.id),
                                        );
                                      }}
                                    >
                                      <ArchiveRestoreIcon aria-hidden="true" />
                                      Restore
                                    </DropdownMenuItem>
                                  )}
                                  {!project.available && (
                                    <DropdownMenuItem
                                      onSelect={(event) => {
                                        event.preventDefault();
                                        void runAction(project.id, () =>
                                          onLocateProject(project.id),
                                        );
                                      }}
                                    >
                                      <FolderSearchIcon aria-hidden="true" />
                                      Locate
                                    </DropdownMenuItem>
                                  )}
                                  <DropdownMenuSub>
                                    <DropdownMenuSubTrigger>
                                      <BotIcon aria-hidden="true" />
                                      Model
                                    </DropdownMenuSubTrigger>
                                    <DropdownMenuSubContent className="w-52">
                                      <DropdownMenuRadioGroup
                                        onValueChange={(value) => {
                                          void updateProject(project.id, {
                                            modelSpec:
                                              value === "default"
                                                ? null
                                                : value,
                                          });
                                        }}
                                        value={project.modelSpec ?? "default"}
                                      >
                                        {modelOptions.map((option) => (
                                          <DropdownMenuRadioItem
                                            key={option.value}
                                            value={option.value}
                                          >
                                            {option.label}
                                          </DropdownMenuRadioItem>
                                        ))}
                                      </DropdownMenuRadioGroup>
                                    </DropdownMenuSubContent>
                                  </DropdownMenuSub>
                                  <DropdownMenuCheckboxItem
                                    checked={project.autoApproveEdits}
                                    onCheckedChange={(checked) => {
                                      void updateProject(project.id, {
                                        autoApproveEdits: checked === true,
                                      });
                                    }}
                                  >
                                    <ShieldCheckIcon aria-hidden="true" />
                                    Auto-approve edits
                                  </DropdownMenuCheckboxItem>
                                  <DropdownMenuSub>
                                    <DropdownMenuSubTrigger>
                                      <PaletteIcon aria-hidden="true" />
                                      Color
                                    </DropdownMenuSubTrigger>
                                    <DropdownMenuSubContent className="w-36">
                                      <DropdownMenuRadioGroup
                                        onValueChange={(value) => {
                                          void updateProject(project.id, {
                                            color:
                                              value === "none" ? null : value,
                                          });
                                        }}
                                        value={project.color ?? "none"}
                                      >
                                        {colorOptions.map((option) => (
                                          <DropdownMenuRadioItem
                                            key={option.value}
                                            value={option.value}
                                          >
                                            <span
                                              className="size-3 rounded-full border border-border"
                                              style={{
                                                backgroundColor: option.color,
                                              }}
                                            />
                                            {option.label}
                                          </DropdownMenuRadioItem>
                                        ))}
                                      </DropdownMenuRadioGroup>
                                    </DropdownMenuSubContent>
                                  </DropdownMenuSub>
                                  <DropdownMenuSub>
                                    <DropdownMenuSubTrigger>
                                      <Settings2Icon aria-hidden="true" />
                                      Icon
                                    </DropdownMenuSubTrigger>
                                    <DropdownMenuSubContent className="w-36">
                                      <DropdownMenuRadioGroup
                                        onValueChange={(value) => {
                                          void updateProject(project.id, {
                                            icon: value,
                                          });
                                        }}
                                        value={project.icon ?? "folder"}
                                      >
                                        {iconOptions.map((option) => (
                                          <DropdownMenuRadioItem
                                            key={option.value}
                                            value={option.value}
                                          >
                                            <option.Icon aria-hidden="true" />
                                            {option.label}
                                          </DropdownMenuRadioItem>
                                        ))}
                                      </DropdownMenuRadioGroup>
                                    </DropdownMenuSubContent>
                                  </DropdownMenuSub>
                                  {!project.archived && (
                                    <>
                                      <DropdownMenuSeparator />
                                      <DropdownMenuItem
                                        disabled={projectIndex <= 0}
                                        onSelect={(event) => {
                                          event.preventDefault();
                                          void runAction("project-order", () =>
                                            reorderActiveProjects(
                                              project.id,
                                              projectIndex - 1,
                                            ),
                                          );
                                        }}
                                      >
                                        <ArrowUpIcon aria-hidden="true" />
                                        Move up
                                      </DropdownMenuItem>
                                      <DropdownMenuItem
                                        disabled={
                                          projectIndex < 0 ||
                                          projectIndex >=
                                            activeProjects.length - 1
                                        }
                                        onSelect={(event) => {
                                          event.preventDefault();
                                          void runAction("project-order", () =>
                                            reorderActiveProjects(
                                              project.id,
                                              projectIndex + 1,
                                            ),
                                          );
                                        }}
                                      >
                                        <ArrowDownIcon aria-hidden="true" />
                                        Move down
                                      </DropdownMenuItem>
                                    </>
                                  )}
                                  <DropdownMenuSeparator />
                                  <DropdownMenuItem
                                    onSelect={(event) => {
                                      event.preventDefault();
                                      startEditingProject(project);
                                    }}
                                  >
                                    <PencilIcon aria-hidden="true" />
                                    Rename
                                  </DropdownMenuItem>
                                  {!project.isDefault && !project.archived && (
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
                            </>
                          )}
                        </SidebarMenuItem>
                        {!project.archived &&
                          !isCollapsed &&
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
                        {!project.archived &&
                          !isCollapsed &&
                          hiddenSessionCount > 0 && (
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
                        {!project.archived &&
                          !isCollapsed &&
                          projectSessions.length === 0 && (
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
              {!loading && (
                <SidebarMenuItem>
                  <SidebarMenuButton
                    className="h-7 text-muted-foreground text-xs"
                    disabled={pendingId === "show-archived-projects"}
                    onClick={() => {
                      void runAction("show-archived-projects", async () => {
                        onShowArchivedProjectsChange(!showArchivedProjects);
                      });
                    }}
                    type="button"
                  >
                    <ArchiveIcon aria-hidden="true" />
                    <span>
                      {showArchivedProjects
                        ? `Hide archived (${archivedProjects.length})`
                        : "Show archived"}
                    </span>
                  </SidebarMenuButton>
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
