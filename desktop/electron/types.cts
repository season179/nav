// Shared shapes for the Electron main process. The backend is a separate Flue
// process reached over HTTP + SSE, so these describe the payloads the main
// process reads — fields are intentionally permissive where the backend owns
// the schema.

// One persisted session as returned by `session.list`.
export type BackendSession = {
  sessionId?: string;
  title?: string | null;
  workspaceRoot?: string | null;
  projectRoot?: string | null;
  updatedAt?: number | string | null;
};

// One event from a session's SSE feed. Only `type` is relied on by the main
// process; the renderer interprets the rest.
export type SessionEvent = {
  type: string;
  session_id?: string;
  event_id?: string;
  error?: string;
  [key: string]: unknown;
};

// A decoded backend response. `sendRpc` keeps the old result envelope for the
// main process while routing requests to the Flue HTTP control plane.
// biome-ignore lint/suspicious/noExplicitAny: the backend owns each method's result schema.
export type RpcResponse<T = any> = {
  jsonrpc?: string;
  id?: string;
  result: T;
  error?: { code?: number; message: string };
};
