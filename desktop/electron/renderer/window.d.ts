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
  text?: string;
  finish_reason?: string;
  status?: string;
};

declare global {
  interface Window {
    nav: {
      onBackendStatus(callback: (status: BackendStatus) => void): () => void;
      onSessionEvent(callback: (event: SessionEvent) => void): () => void;
    };
  }
}
