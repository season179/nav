import { useQuery } from "@tanstack/react-query";
import { useEffect, useMemo, useState } from "react";
import {
  EMPTY_STACKS_RESULT,
  sessionStacksQueryOptions,
} from "../lib/nav-queries.ts";
import type { StackEntry, StackRequest, StackResponse } from "../types.ts";

export default function StacksPage({
  onUnavailable,
  sessionId,
}: {
  onUnavailable?: (reason: string) => void;
  sessionId: string | null;
}) {
  const stacksQuery = useQuery(sessionStacksQueryOptions(sessionId));
  const result = stacksQuery.data ?? EMPTY_STACKS_RESULT;
  const stacks = Array.isArray(result.stacks) ? result.stacks : [];
  const unavailableReason = result.unavailableReason ?? null;

  useEffect(() => {
    if (sessionId && stacks.length === 0 && unavailableReason) {
      onUnavailable?.(unavailableReason);
    }
  }, [onUnavailable, sessionId, stacks.length, unavailableReason]);

  const orderedStacks = useMemo(
    () => [...stacks].sort((left, right) => left.sequence - right.sequence),
    [stacks],
  );

  if (!sessionId) {
    return (
      <section className="stacks-page" aria-label="Model call stacks">
        <EmptyStacks text="No session selected" />
      </section>
    );
  }

  if (stacksQuery.error) {
    return (
      <section className="stacks-page" aria-label="Model call stacks">
        <div className="stacks-error">
          Could not load stacks: {errorMessage(stacksQuery.error)}
        </div>
      </section>
    );
  }

  return (
    <section className="stacks-page" aria-label="Model call stacks">
      <div className="stacks-header">
        <div>
          <h1>Stacks</h1>
          <p className="stacks-subtitle">
            {stacksQuery.isPending
              ? "Loading"
              : `${orderedStacks.length} model call${
                  orderedStacks.length === 1 ? "" : "s"
                }`}
          </p>
        </div>
      </div>

      {orderedStacks.length === 0 ? (
        <EmptyStacks text={emptyStackText(unavailableReason)} />
      ) : (
        <ol className="stack-call-list">
          {orderedStacks.map((stack) => (
            <StackCall key={stack.id} stack={stack} />
          ))}
        </ol>
      )}
    </section>
  );
}

function StackCall({ stack }: { stack: StackEntry }) {
  const request = stack.request ?? ({} as StackRequest);
  const response = stack.response ?? ({} as StackResponse);
  const tokens = tokenSummary(response.tokenUsage);

  return (
    <li className={`stack-call stack-call-${stack.status}`}>
      <header className="stack-call-header">
        <div>
          <h2>Call {stack.sequence + 1}</h2>
          <p>
            {stack.status} · {formatDuration(stack.durationMs)} ·{" "}
            {formatTime(stack.startedAtMs)}
          </p>
        </div>
        <div className="stack-call-meta">
          {request.model ? (
            <span className="stack-call-model">{request.model}</span>
          ) : null}
          {Number.isFinite(response.statusCode) ? (
            <span className="stack-call-status-code">
              HTTP {response.statusCode}
            </span>
          ) : null}
          {tokens ? <span className="stack-call-tokens">{tokens}</span> : null}
          <span className="stack-run-id" title={stack.runId}>
            {shortId(stack.runId)}
          </span>
        </div>
      </header>

      <RequestSection request={request} />
      <ResponseSection response={response} />
    </li>
  );
}

function RequestSection({ request }: { request: StackRequest }) {
  // Serialize the body only once the user opens the section: a session can hold
  // hundreds of calls, each carrying the full conversation context, so eagerly
  // pretty-printing every body on first paint is wasted work.
  const [expanded, setExpanded] = useState(false);
  const hasBody = request.body !== undefined && request.body !== null;
  const meta = [request.api, request.url].filter(Boolean).join(" · ");

  return (
    <details
      className="stack-section stack-section-request"
      onToggle={(event) => setExpanded(event.currentTarget.open)}
    >
      <summary>
        <span className="stack-section-title">Request</span>
        {meta ? <span className="stack-section-meta">{meta}</span> : null}
      </summary>
      {expanded && hasBody ? (
        <pre className="stack-json">{stringifyJson(request.body)}</pre>
      ) : null}
      {!hasBody ? (
        <p className="stack-section-empty">No request body captured.</p>
      ) : null}
    </details>
  );
}

function ResponseSection({ response }: { response: StackResponse }) {
  // Defer body serialization until the section is opened (see RequestSection).
  const [expanded, setExpanded] = useState(false);
  const hasBody = response.body !== undefined && response.body !== null;

  return (
    <details
      className="stack-section stack-section-response"
      onToggle={(event) => setExpanded(event.currentTarget.open)}
    >
      <summary>
        <span className="stack-section-title">Response</span>
        {Number.isFinite(response.statusCode) ? (
          <span className="stack-section-meta">HTTP {response.statusCode}</span>
        ) : null}
      </summary>
      {response.error ? (
        <pre className="stack-error-body">{response.error}</pre>
      ) : null}
      {expanded && hasBody ? (
        <pre className="stack-json">{stringifyJson(response.body)}</pre>
      ) : null}
      {!hasBody && !response.error ? (
        <p className="stack-section-empty">No response body captured.</p>
      ) : null}
    </details>
  );
}

function EmptyStacks({ text }: { text: string }) {
  return <div className="stacks-empty">{text}</div>;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function emptyStackText(reason: string | null): string {
  switch (reason) {
    case "trimmed_or_missing":
      return "Stack records for this session were no longer available. The stack log is capped at 800MB, so older records may have been trimmed.";
    case "stack_store_unavailable":
      return "Stack storage is unavailable for this backend run.";
    case "stack_store_error":
      return "Stack records could not be read from the local stack log.";
    default:
      return "No model calls captured for this live session yet";
  }
}

function tokenSummary(usage: unknown): string {
  if (!usage || typeof usage !== "object") {
    return "";
  }
  const counts = usage as {
    input?: number;
    output?: number;
    total?: number;
  };
  const parts: string[] = [];
  if (Number.isFinite(counts.input)) {
    parts.push(`${counts.input} in`);
  }
  if (Number.isFinite(counts.output)) {
    parts.push(`${counts.output} out`);
  }
  if (Number.isFinite(counts.total)) {
    parts.push(`${counts.total} total`);
  }
  return parts.join(" / ");
}

function stringifyJson(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

function formatDuration(durationMs: number): string {
  if (!Number.isFinite(durationMs)) {
    return "0 ms";
  }
  if (durationMs < 1000) {
    return `${durationMs.toFixed(1)} ms`;
  }
  return `${(durationMs / 1000).toFixed(2)} s`;
}

function formatTime(ms: number): string {
  const date = new Date(ms);
  if (Number.isNaN(date.getTime())) {
    return "unknown time";
  }
  return date.toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

function shortId(id: string): string {
  if (!id) {
    return "";
  }
  return id.length > 8 ? id.slice(0, 8) : id;
}
