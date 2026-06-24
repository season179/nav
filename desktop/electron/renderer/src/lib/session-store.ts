import { useSelector } from "@tanstack/react-store";
import { createStore } from "@tanstack/store";
import type { ChatMessage, SessionEvent, SessionState } from "../types.ts";
import {
  appendMessage,
  createSessionState,
  reduceSessions,
} from "./session-runtime.ts";

export type SessionStateMap = Record<string, SessionState>;

export const sessionStateStore = createStore<SessionStateMap>({});

export function useSessionStates<TSelected>(
  selector: (states: SessionStateMap) => TSelected,
): TSelected {
  return useSelector(sessionStateStore, selector);
}

export function updateStoredSessionState(
  sessionId: string | null | undefined,
  updater: (state: SessionState) => SessionState,
) {
  if (!sessionId) {
    return;
  }

  sessionStateStore.setState((current) => {
    const previous = current[sessionId] ?? createSessionState();
    const next = updater(previous);
    if (next === previous) {
      return current;
    }
    return { ...current, [sessionId]: next };
  });
}

export function appendStoredSessionMessage(
  sessionId: string,
  role: ChatMessage["role"],
  text?: string,
) {
  updateStoredSessionState(sessionId, (state) =>
    appendMessage(state, role, text),
  );
}

export function applyStoredSessionEvent(event: SessionEvent) {
  sessionStateStore.setState((current) => reduceSessions(current, event));
}

export function resetSessionStateStoreForTests() {
  sessionStateStore.setState(() => ({}));
}
