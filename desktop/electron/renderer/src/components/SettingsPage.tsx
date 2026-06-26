import { useForm } from "@tanstack/react-form";
import {
  type ColumnDef,
  type FilterFn,
  flexRender,
  getCoreRowModel,
  getFilteredRowModel,
  getSortedRowModel,
  type SortingState,
  useReactTable,
} from "@tanstack/react-table";
import { CheckIcon } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { cn } from "@/lib/utils";
import {
  formatThinkingLabel,
  modelInfoKey,
  modelOptionKey,
  modelOptionMatchesQuery,
  sessionModeLabel,
  settingsFormDefaults,
  settingsSessionModeOptions,
  thinkingLevelsFor,
} from "../lib/settings-model.ts";
import type { ModelInfo, ModelOption, SessionMode } from "../types.ts";

export default function SettingsPage({
  connected,
  modelInfo,
  modelOptions,
  modelSwitching,
  newSessionMode,
  sessionId,
  onModelChange,
  onNewSessionModeChange,
  onThinkingChange,
}: {
  connected: boolean;
  modelInfo: ModelInfo | null;
  modelOptions: ModelOption[];
  modelSwitching: boolean;
  newSessionMode: SessionMode;
  sessionId: string | null;
  onModelChange: (option: ModelOption) => void | Promise<void>;
  onNewSessionModeChange: (mode: SessionMode) => void;
  onThinkingChange: (level: string) => void | Promise<void>;
}) {
  const selectedModelKey = modelInfoKey(modelInfo);
  const thinkingLevels = thinkingLevelsFor(modelInfo);
  const [globalFilter, setGlobalFilter] = useState("");
  const [sorting, setSorting] = useState<SortingState>([
    { id: "label", desc: false },
  ]);
  const form = useForm({
    defaultValues: settingsFormDefaults(newSessionMode, modelInfo),
  });

  useEffect(() => {
    form.reset(settingsFormDefaults(newSessionMode, modelInfo));
  }, [form, modelInfo, newSessionMode]);

  const columns = useMemo<ColumnDef<ModelOption>[]>(
    () => [
      {
        accessorKey: "label",
        header: "Model",
        cell: (info) => String(info.getValue()),
      },
      {
        accessorKey: "provider",
        header: "Provider",
        cell: (info) => String(info.getValue()),
      },
      {
        accessorFn: (option) =>
          Array.isArray(option.thinkingLevels)
            ? option.thinkingLevels.join(", ")
            : "",
        id: "thinkingLevels",
        header: "Thinking",
        cell: (info) => String(info.getValue() || "default"),
      },
    ],
    [],
  );
  const modelFilter = useMemo<FilterFn<ModelOption>>(
    () => (row, _columnId, filterValue) =>
      modelOptionMatchesQuery(row.original, String(filterValue ?? "")),
    [],
  );
  const table = useReactTable({
    columns,
    data: modelOptions,
    getCoreRowModel: getCoreRowModel(),
    getFilteredRowModel: getFilteredRowModel(),
    getSortedRowModel: getSortedRowModel(),
    globalFilterFn: modelFilter,
    onGlobalFilterChange: setGlobalFilter,
    onSortingChange: setSorting,
    state: {
      globalFilter,
      sorting,
    },
  });
  const modelSelectionDisabled = !connected || !sessionId || modelSwitching;

  return (
    <section
      className="min-h-0 flex-1 overflow-y-auto bg-background px-5 py-6"
      aria-label="Settings"
    >
      <div className="mx-auto w-full max-w-5xl space-y-6">
        <header className="flex items-center justify-between gap-4">
          <div>
            <h1 className="font-semibold text-2xl tracking-tight">Settings</h1>
            <p className="text-muted-foreground text-sm">
              {sessionId ? shortId(sessionId) : "No thread selected"}
            </p>
          </div>
        </header>

        <form
          className="space-y-4"
          onSubmit={(event) => {
            event.preventDefault();
            event.stopPropagation();
            void form.handleSubmit();
          }}
        >
          <section
            className="grid gap-4 rounded-lg border bg-card p-4 shadow-sm sm:grid-cols-[minmax(0,1fr)_auto] sm:items-center"
            aria-labelledby="mode-heading"
          >
            <div className="space-y-1">
              <h2 id="mode-heading" className="font-medium">
                Start in
              </h2>
              <span className="text-muted-foreground text-sm">
                {sessionModeLabel(newSessionMode)}
              </span>
            </div>
            <form.Field name="mode">
              {(field) => (
                <div
                  className="grid grid-cols-2 gap-1 rounded-lg border bg-muted p-1"
                  role="radiogroup"
                >
                  {settingsSessionModeOptions.map((option) => {
                    const selected = field.state.value === option.value;
                    return (
                      <label
                        key={option.value}
                        className={cn(
                          "flex h-9 min-w-28 items-center justify-center rounded-md px-3 font-medium text-sm transition-colors",
                          selected
                            ? "bg-background text-foreground shadow-sm"
                            : "text-muted-foreground hover:text-foreground",
                          !connected && "cursor-not-allowed opacity-50",
                        )}
                        data-disabled={!connected || undefined}
                        data-selected={selected || undefined}
                      >
                        <input
                          type="radio"
                          className="sr-only"
                          name={field.name}
                          value={option.value}
                          checked={selected}
                          disabled={!connected}
                          onBlur={field.handleBlur}
                          onChange={() => {
                            field.handleChange(option.value);
                            onNewSessionModeChange(option.value);
                          }}
                        />
                        <span>{option.label}</span>
                      </label>
                    );
                  })}
                </div>
              )}
            </form.Field>
          </section>

          <section
            className="grid gap-4 rounded-lg border bg-card p-4 shadow-sm sm:grid-cols-[minmax(0,1fr)_14rem] sm:items-center"
            aria-labelledby="thinking-heading"
          >
            <div className="space-y-1">
              <h2 id="thinking-heading" className="font-medium">
                Thinking
              </h2>
              <span className="text-muted-foreground text-sm">
                {formatThinkingLabel(modelInfo?.thinking ?? "")}
              </span>
            </div>
            <form.Field name="thinking">
              {(field) => (
                <Select
                  value={field.state.value}
                  disabled={modelSelectionDisabled || thinkingLevels.length < 2}
                  onValueChange={(level) => {
                    field.handleChange(level);
                    onThinkingChange(level);
                  }}
                >
                  <SelectTrigger className="w-full" onBlur={field.handleBlur}>
                    <SelectValue placeholder="Default" />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectGroup>
                      {thinkingLevels.length === 0 ? (
                        <SelectItem value="default">Default</SelectItem>
                      ) : (
                        thinkingLevels.map((level) => (
                          <SelectItem key={level} value={level}>
                            {formatThinkingLabel(level)}
                          </SelectItem>
                        ))
                      )}
                    </SelectGroup>
                  </SelectContent>
                </Select>
              )}
            </form.Field>
          </section>

          <section className="rounded-lg border bg-card p-4 shadow-sm">
            <div className="flex flex-wrap items-center justify-between gap-3">
              <div className="space-y-1">
                <h2 className="font-medium">Models</h2>
                <span className="text-muted-foreground text-sm">
                  {modelOptions.length} option
                  {modelOptions.length === 1 ? "" : "s"}
                </span>
              </div>
              <div className="w-full sm:w-72">
                <Input
                  type="search"
                  aria-label="Search models"
                  placeholder="Search models"
                  value={globalFilter}
                  onChange={(event) => setGlobalFilter(event.target.value)}
                />
              </div>
            </div>
            <form.Field
              name="modelKey"
              validators={{
                onChange: ({ value }) =>
                  value &&
                  !modelOptions.some(
                    (option) => modelOptionKey(option) === value,
                  )
                    ? "Selected model is no longer available"
                    : undefined,
              }}
            >
              {(field) => (
                <>
                  <div className="mt-4 overflow-hidden rounded-lg border">
                    <table className="w-full border-collapse text-sm">
                      <thead>
                        {table.getHeaderGroups().map((headerGroup) => (
                          <tr
                            key={headerGroup.id}
                            className="border-b bg-muted/60 text-muted-foreground"
                          >
                            <th className="w-10 px-3 py-2" />
                            {headerGroup.headers.map((header) => (
                              <th
                                key={header.id}
                                className="px-3 py-2 text-left font-medium"
                                colSpan={header.colSpan}
                              >
                                {header.isPlaceholder ? null : (
                                  <Button
                                    type="button"
                                    className="h-7 justify-start gap-1 px-2 text-muted-foreground hover:text-foreground"
                                    variant="ghost"
                                    size="sm"
                                    disabled={!header.column.getCanSort()}
                                    onClick={header.column.getToggleSortingHandler()}
                                  >
                                    {flexRender(
                                      header.column.columnDef.header,
                                      header.getContext(),
                                    )}
                                    <span aria-hidden="true">
                                      {sortMarker(header.column.getIsSorted())}
                                    </span>
                                  </Button>
                                )}
                              </th>
                            ))}
                          </tr>
                        ))}
                      </thead>
                      <tbody>
                        {table.getRowModel().rows.map((row) => {
                          const option = row.original;
                          const optionKey = modelOptionKey(option);
                          const selected = optionKey === selectedModelKey;
                          return (
                            <tr
                              key={row.id}
                              className="border-b transition-colors last:border-b-0 hover:bg-muted/50 data-[selected=true]:bg-accent"
                              data-selected={selected || undefined}
                            >
                              <td className="px-3 py-2 text-primary">
                                {selected ? (
                                  <CheckIcon className="size-4" />
                                ) : null}
                              </td>
                              {row.getVisibleCells().map((cell) => (
                                <td key={cell.id} className="min-w-0 px-3 py-2">
                                  {cell.column.id === "label" ? (
                                    <Button
                                      type="button"
                                      className="h-auto max-w-full justify-start p-0 text-left font-medium hover:bg-transparent"
                                      variant="ghost"
                                      disabled={modelSelectionDisabled}
                                      onClick={() => {
                                        field.handleChange(optionKey);
                                        onModelChange(option);
                                      }}
                                    >
                                      {flexRender(
                                        cell.column.columnDef.cell,
                                        cell.getContext(),
                                      )}
                                    </Button>
                                  ) : (
                                    flexRender(
                                      cell.column.columnDef.cell,
                                      cell.getContext(),
                                    )
                                  )}
                                </td>
                              ))}
                            </tr>
                          );
                        })}
                      </tbody>
                    </table>
                    {table.getRowModel().rows.length === 0 ? (
                      <div className="px-4 py-8 text-center text-muted-foreground text-sm">
                        No matching models
                      </div>
                    ) : null}
                  </div>
                  {!field.state.meta.isValid ? (
                    <div className="mt-3 text-destructive text-sm" role="alert">
                      {field.state.meta.errors.join(", ")}
                    </div>
                  ) : null}
                </>
              )}
            </form.Field>
          </section>
        </form>
      </div>
    </section>
  );
}

function sortMarker(direction: false | "asc" | "desc"): string {
  if (direction === "asc") {
    return "↑";
  }
  if (direction === "desc") {
    return "↓";
  }
  return "";
}

function shortId(id: string): string {
  return id.length > 8 ? id.slice(0, 8) : id;
}
