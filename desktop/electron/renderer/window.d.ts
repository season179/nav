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
  updatedAt: number;
};

declare global {
  interface Window {
    nav: {
      onBackendStatus(callback: (status: BackendStatus) => void): () => void;
      onSessionEvent(callback: (event: SessionEvent) => void): () => void;
      sessionSendMessage(text: string): Promise<void>;
      listSessions(): Promise<SessionSummary[]>;
      modelInfo(sessionId?: string): Promise<{
        label: string;
        thinking?: string | null;
        tokenUsage?: { used: number; contextWindow: number } | null;
      }>;
      switchSession(sessionId: string): Promise<void>;
      newSession(): Promise<string>;
    };
  }
}
