export {};

type BackendStatus = {
  state: string;
  backendUrl?: string;
  sessionId?: string;
  message?: string;
};

type SessionEvent = {
  event_id: string;
  session_id: string;
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

type SessionSummary = {
  sessionId: string;
  title: string | null;
  workspaceRoot: string | null;
  projectRoot: string | null;
  updatedAt: number;
};

type ModelOption = {
  provider: string;
  model: string;
  label: string;
};

type ModelInfo = {
  label: string;
  provider?: string | null;
  model?: string | null;
  thinking?: string | null;
  tokenUsage?: { used: number; contextWindow: number } | null;
};

declare global {
  interface Window {
    nav: {
      onBackendStatus(callback: (status: BackendStatus) => void): () => void;
      onSessionEvent(callback: (event: SessionEvent) => void): () => void;
      sessionSendMessage(text: string): Promise<void>;
      sessionStop(): Promise<boolean>;
      listSessions(): Promise<SessionSummary[]>;
      createProject(mode?: "local" | "worktree" | null): Promise<string | null>;
      modelInfo(sessionId?: string): Promise<ModelInfo>;
      modelList(): Promise<ModelOption[]>;
      switchModel(provider: string, model: string): Promise<ModelInfo>;
      sessionStacks(sessionId?: string): Promise<{
        stacks: Array<{
          id: string;
          runId: string;
          sequence: number;
          status: string;
          startedAtMs: number;
          durationMs: number;
          layers: Array<{
            kind: string;
            title: string;
            status: string;
            summary: string;
            entries: Array<{ label: string; value: string }>;
            text?: string;
            json?: unknown;
          }>;
        }>;
        unavailableReason?: string;
      }>;
      sessionStackAvailability(sessionId?: string): Promise<{
        available: boolean;
      }>;
      switchSession(sessionId: string): Promise<void>;
      newSession(
        workspaceRoot?: string | null,
        mode?: "local" | "worktree" | null,
      ): Promise<string | null>;
    };
  }
}
