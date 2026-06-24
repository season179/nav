// Shared renderer types: the wire shapes exchanged with the main process over
// the preload `window.nav` bridge, plus the per-session view state the renderer
// derives from them. The global `Window.nav` augmentation lives here too, so any
// file that touches `window.nav` sees the typed API.

export type SessionMode = "local" | "worktree";

export type BackendStatus = {
  state: string;
  backendUrl?: string;
  sessionId?: string;
  message?: string;
};

export type SessionEvent = {
  event_id?: string;
  session_id?: string;
  type: string;
  sequence?: number;
  run_id?: string;
  message_id?: string;
  role?: string;
  text?: string;
  status?: string;
  error?: string;
  tool_call_id?: string;
  tool_name?: string;
};

export type SessionSummary = {
  sessionId: string;
  title: string | null;
  workspaceRoot: string | null;
  projectRoot: string | null;
  updatedAt: number;
};

export type ModelOption = {
  provider: string;
  model: string;
  label: string;
  thinkingLevels?: string[];
};

export type TokenUsage = {
  used: number;
  contextWindow: number;
};

export type ModelInfo = {
  label: string;
  provider?: string | null;
  model?: string | null;
  thinking?: string | null;
  thinkingLevels?: string[];
  tokenUsage?: TokenUsage | null;
};

export type StackRequest = {
  api: string;
  url: string;
  model: string;
  body?: unknown;
};

export type StackResponse = {
  statusCode?: number;
  body?: unknown;
  error?: string;
  tokenUsage?: unknown;
};

export type StackEntry = {
  id: string;
  runId: string;
  sequence: number;
  status: string;
  startedAtMs: number;
  durationMs: number;
  request?: StackRequest;
  response?: StackResponse;
};

export type SessionStacksResult = {
  stacks: StackEntry[];
  unavailableReason?: string;
};

export type StackAvailabilityResult = {
  available: boolean;
};

// One transcript line. Chat bubbles (user/assistant/error) and tool lines share
// an `id` but otherwise differ, so they form a discriminated union on `role`.
export type ChatMessage = {
  id: string;
  role: "user" | "assistant" | "error";
  text: string;
  createdAt: string;
};

export type ToolMessage = {
  id: string;
  role: "tool";
  toolCallId: string;
  state: string;
  toolName: string;
  detail: string;
};

export type Message = ChatMessage | ToolMessage;

// One session's view state, keyed by session id in the renderer.
export type SessionState = {
  messages: Message[];
  running: boolean;
  stopPending: boolean;
  modelInfo: ModelInfo | null;
  stackAvailable: boolean;
  stackRefreshKey: number;
  messageSeq: number;
  streamingAssistantMessageId: string | null;
};

// The API the preload exposes on `window.nav`.
export type NavApi = {
  onBackendStatus(callback: (status: BackendStatus) => void): () => void;
  onSessionEvent(callback: (event: SessionEvent) => void): () => void;
  sessionSendMessage(sessionId: string, text: string): Promise<void>;
  sessionStop(sessionId: string): Promise<boolean>;
  listSessions(): Promise<SessionSummary[]>;
  createProject(mode?: SessionMode | null): Promise<string | null>;
  modelInfo(sessionId?: string): Promise<ModelInfo>;
  modelList(): Promise<ModelOption[]>;
  switchModel(
    sessionId: string,
    provider: string,
    model: string,
    thinkingLevel?: string | null,
  ): Promise<ModelInfo>;
  switchThinking(sessionId: string, thinkingLevel: string): Promise<ModelInfo>;
  sessionStacks(sessionId?: string): Promise<SessionStacksResult>;
  sessionStackAvailability(
    sessionId?: string,
  ): Promise<StackAvailabilityResult>;
  switchSession(sessionId: string): Promise<void>;
  newSession(
    workspaceRoot?: string | null,
    mode?: SessionMode | null,
  ): Promise<string | null>;
  getSessionMode(): Promise<SessionMode | null>;
  setSessionMode(mode: SessionMode): Promise<void>;
};

declare global {
  interface Window {
    nav: NavApi;
  }
}
