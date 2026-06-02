import { useEffect, useMemo, useState } from "react";

export default function StacksPage({ onUnavailable, sessionId }) {
  const [stacks, setStacks] = useState([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState(null);
  const [unavailableReason, setUnavailableReason] = useState(null);

  useEffect(() => {
    let alive = true;
    if (!sessionId || !window.nav) {
      setStacks([]);
      setUnavailableReason(null);
      return undefined;
    }

    setLoading(true);
    setError(null);
    setUnavailableReason(null);
    window.nav
      .sessionStacks(sessionId)
      .then((result) => {
        if (alive) {
          const nextStacks = Array.isArray(result?.stacks) ? result.stacks : [];
          setStacks(nextStacks);
          setUnavailableReason(result?.unavailableReason ?? null);
          if (nextStacks.length === 0 && result?.unavailableReason) {
            onUnavailable?.(result.unavailableReason);
          }
        }
      })
      .catch((fetchError) => {
        if (alive) {
          setError(fetchError.message);
        }
      })
      .finally(() => {
        if (alive) {
          setLoading(false);
        }
      });

    return () => {
      alive = false;
    };
  }, [onUnavailable, sessionId]);

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

  if (error) {
    return (
      <section className="stacks-page" aria-label="Model call stacks">
        <div className="stacks-error">Could not load stacks: {error}</div>
      </section>
    );
  }

  return (
    <section className="stacks-page" aria-label="Model call stacks">
      <div className="stacks-header">
        <div>
          <h1>Stacks</h1>
          <p className="stacks-subtitle">
            {loading
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

function StackCall({ stack }) {
  const request = stack.request ?? {};
  const response = stack.response ?? {};
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

function RequestSection({ request }) {
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

function ResponseSection({ response }) {
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

function EmptyStacks({ text }) {
  return <div className="stacks-empty">{text}</div>;
}

function emptyStackText(reason) {
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

function tokenSummary(usage) {
  if (!usage || typeof usage !== "object") {
    return "";
  }
  const parts = [];
  if (Number.isFinite(usage.input)) {
    parts.push(`${usage.input} in`);
  }
  if (Number.isFinite(usage.output)) {
    parts.push(`${usage.output} out`);
  }
  if (Number.isFinite(usage.total)) {
    parts.push(`${usage.total} total`);
  }
  return parts.join(" / ");
}

function stringifyJson(value) {
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

function formatDuration(durationMs) {
  if (!Number.isFinite(durationMs)) {
    return "0 ms";
  }
  if (durationMs < 1000) {
    return `${durationMs.toFixed(1)} ms`;
  }
  return `${(durationMs / 1000).toFixed(2)} s`;
}

function formatTime(ms) {
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

function shortId(id) {
  if (!id) {
    return "";
  }
  return id.length > 8 ? id.slice(0, 8) : id;
}
