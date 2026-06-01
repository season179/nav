export const STACK_AVAILABILITY_RECHECK_DELAY_MS = 120;

const TERMINAL_RUN_EVENTS = new Set([
  "run.completed",
  "run.failed",
  "run.cancelled",
]);

export function shouldRefreshStackAvailabilityForEvent(eventType) {
  return TERMINAL_RUN_EVENTS.has(eventType);
}
