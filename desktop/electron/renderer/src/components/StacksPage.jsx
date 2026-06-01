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
  return (
    <li className={`stack-call stack-call-${stack.status}`}>
      <header className="stack-call-header">
        <div>
          <h2>Call {stack.sequence + 1}</h2>
          <p>
            {stack.status} - {formatDuration(stack.durationMs)} -{" "}
            {formatTime(stack.startedAtMs)}
          </p>
        </div>
        <span className="stack-run-id" title={stack.runId}>
          {shortId(stack.runId)}
        </span>
      </header>

      <ol className="stack-layer-list">
        {stack.layers.map((layer, index) => (
          <StackLayer
            index={index}
            key={`${stack.id}-${layer.kind}`}
            layer={layer}
          />
        ))}
      </ol>
    </li>
  );
}

function StackLayer({ index, layer }) {
  return (
    <li className={`stack-layer stack-layer-${layer.status}`}>
      <details open={defaultOpen(layer)}>
        <summary>
          <span className="stack-layer-index">
            {String(index + 1).padStart(2, "0")}
          </span>
          <span className="stack-layer-title">{layer.title}</span>
          <span className="stack-layer-status">{layer.status}</span>
        </summary>
        <p className="stack-layer-summary">{layer.summary}</p>
        {layer.entries?.length ? <EntryGrid entries={layer.entries} /> : null}
        {layer.text ? <pre className="stack-text">{layer.text}</pre> : null}
        {layer.json !== undefined ? (
          <pre className="stack-json">{stringifyJson(layer.json)}</pre>
        ) : null}
      </details>
    </li>
  );
}

function EntryGrid({ entries }) {
  return (
    <dl className="stack-entry-grid">
      {entries.map((entry) => (
        <div key={`${entry.label}:${entry.value}`} className="stack-entry">
          <dt>{entry.label}</dt>
          <dd>{entry.value}</dd>
        </div>
      ))}
    </dl>
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

function defaultOpen(layer) {
  return [
    "system_prompt",
    "session_history",
    "provider_payload",
    "normalized_response",
    "carried_forward",
  ].includes(layer.kind);
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
