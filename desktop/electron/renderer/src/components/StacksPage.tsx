import { useQuery } from "@tanstack/react-query";
import {
  type ColumnDef,
  getCoreRowModel,
  getSortedRowModel,
  type SortingState,
  useReactTable,
} from "@tanstack/react-table";
import { useEffect, useMemo, useState } from "react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
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
  const [sorting, setSorting] = useState<SortingState>([
    { id: "sequence", desc: false },
  ]);

  useEffect(() => {
    if (sessionId && stacks.length === 0 && unavailableReason) {
      onUnavailable?.(unavailableReason);
    }
  }, [onUnavailable, sessionId, stacks.length, unavailableReason]);

  const columns = useMemo<ColumnDef<StackEntry>[]>(
    () => [
      {
        accessorKey: "sequence",
        header: "Call",
      },
      {
        accessorKey: "status",
        header: "Status",
      },
      {
        accessorFn: (stack) => stack.request?.model ?? "",
        id: "model",
        header: "Model",
      },
      {
        accessorKey: "durationMs",
        header: "Duration",
      },
      {
        accessorKey: "startedAtMs",
        header: "Started",
      },
    ],
    [],
  );
  const stackTable = useReactTable({
    columns,
    data: stacks,
    getCoreRowModel: getCoreRowModel(),
    getSortedRowModel: getSortedRowModel(),
    onSortingChange: setSorting,
    state: {
      sorting,
    },
  });
  const stackRows = stackTable.getRowModel().rows;

  if (!sessionId) {
    return (
      <section
        className="min-h-0 flex-1 overflow-y-auto bg-background px-5 py-6"
        aria-label="Model call stacks"
      >
        <EmptyStacks text="No session selected" />
      </section>
    );
  }

  if (stacksQuery.error) {
    return (
      <section
        className="min-h-0 flex-1 overflow-y-auto bg-background px-5 py-6"
        aria-label="Model call stacks"
      >
        <div className="mx-auto max-w-5xl rounded-lg border border-destructive/30 bg-destructive/10 p-4 text-destructive text-sm">
          Could not load stacks: {errorMessage(stacksQuery.error)}
        </div>
      </section>
    );
  }

  return (
    <section
      className="min-h-0 flex-1 overflow-y-auto bg-background px-5 py-6"
      aria-label="Model call stacks"
    >
      <div className="mx-auto flex w-full max-w-5xl flex-wrap items-center justify-between gap-4">
        <div>
          <h1 className="font-semibold text-2xl tracking-tight">Stacks</h1>
          <p className="text-muted-foreground text-sm">
            {stacksQuery.isPending
              ? "Loading"
              : `${stackRows.length} model call${
                  stackRows.length === 1 ? "" : "s"
                }`}
          </p>
        </div>
        {stacks.length > 1 ? (
          <fieldset className="flex flex-wrap gap-2" aria-label="Sort stacks">
            {(["sequence", "status", "durationMs"] as const).map((columnId) => {
              const column = stackTable.getColumn(columnId);
              const sorted = column?.getIsSorted() ?? false;
              return (
                <Button
                  key={columnId}
                  type="button"
                  variant={sorted ? "secondary" : "outline"}
                  size="sm"
                  aria-pressed={Boolean(sorted)}
                  onClick={() => column?.toggleSorting(sorted === "asc")}
                >
                  {sortButtonLabel(columnId, sorted)}
                </Button>
              );
            })}
          </fieldset>
        ) : null}
      </div>

      {stackRows.length === 0 ? (
        <EmptyStacks text={emptyStackText(unavailableReason)} />
      ) : (
        <ol className="mx-auto mt-5 w-full max-w-5xl space-y-3">
          {stackRows.map((row) => (
            <StackCall key={row.original.id} stack={row.original} />
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
    <li
      className={cn(
        "rounded-lg border bg-card p-4 shadow-sm",
        stack.status === "error" ? "border-destructive/40" : "border-border",
      )}
    >
      <header className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h2 className="font-medium">Call {stack.sequence + 1}</h2>
          <p className="text-muted-foreground text-sm">
            {stack.status} · {formatDuration(stack.durationMs)} ·{" "}
            {formatTime(stack.startedAtMs)}
          </p>
        </div>
        <div className="flex flex-wrap justify-end gap-2">
          {request.model ? (
            <Badge variant="secondary">{request.model}</Badge>
          ) : null}
          {Number.isFinite(response.statusCode) ? (
            <Badge variant="outline">HTTP {response.statusCode}</Badge>
          ) : null}
          {tokens ? <Badge variant="outline">{tokens}</Badge> : null}
          <Badge variant="ghost" title={stack.runId}>
            {shortId(stack.runId)}
          </Badge>
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
      className="mt-3 rounded-md border bg-muted/30"
      onToggle={(event) => setExpanded(event.currentTarget.open)}
    >
      <summary className="flex cursor-default items-center justify-between gap-3 px-3 py-2 text-sm">
        <span className="font-medium">Request</span>
        {meta ? (
          <span className="truncate text-muted-foreground text-xs">{meta}</span>
        ) : null}
      </summary>
      {expanded && hasBody ? (
        <pre className="max-h-96 overflow-auto border-t bg-background p-3 text-xs leading-5">
          {stringifyJson(request.body)}
        </pre>
      ) : null}
      {!hasBody ? (
        <p className="border-t px-3 py-2 text-muted-foreground text-sm">
          No request body captured.
        </p>
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
      className="mt-3 rounded-md border bg-muted/30"
      onToggle={(event) => setExpanded(event.currentTarget.open)}
    >
      <summary className="flex cursor-default items-center justify-between gap-3 px-3 py-2 text-sm">
        <span className="font-medium">Response</span>
        {Number.isFinite(response.statusCode) ? (
          <span className="text-muted-foreground text-xs">
            HTTP {response.statusCode}
          </span>
        ) : null}
      </summary>
      {response.error ? (
        <pre className="overflow-auto border-t bg-destructive/10 p-3 text-destructive text-xs leading-5">
          {response.error}
        </pre>
      ) : null}
      {expanded && hasBody ? (
        <pre className="max-h-96 overflow-auto border-t bg-background p-3 text-xs leading-5">
          {stringifyJson(response.body)}
        </pre>
      ) : null}
      {!hasBody && !response.error ? (
        <p className="border-t px-3 py-2 text-muted-foreground text-sm">
          No response body captured.
        </p>
      ) : null}
    </details>
  );
}

function EmptyStacks({ text }: { text: string }) {
  return (
    <div className="mx-auto flex min-h-[45vh] max-w-5xl items-center justify-center rounded-lg border border-dashed bg-card px-6 text-center text-muted-foreground text-sm">
      {text}
    </div>
  );
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

function sortButtonLabel(
  columnId: "sequence" | "status" | "durationMs",
  sorted: false | "asc" | "desc",
): string {
  const label =
    columnId === "sequence"
      ? "Call"
      : columnId === "durationMs"
        ? "Duration"
        : "Status";
  if (sorted === "asc") {
    return `${label} asc`;
  }
  if (sorted === "desc") {
    return `${label} desc`;
  }
  return label;
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
