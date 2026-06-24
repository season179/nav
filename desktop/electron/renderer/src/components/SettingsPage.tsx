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
import { useEffect, useMemo, useState } from "react";
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
    <section className="settings-page" aria-label="Settings">
      <div className="settings-content">
        <header className="settings-header">
          <div>
            <h1>Settings</h1>
            <p className="settings-subtitle">
              {sessionId ? shortId(sessionId) : "No thread selected"}
            </p>
          </div>
        </header>

        <form
          className="settings-form"
          onSubmit={(event) => {
            event.preventDefault();
            event.stopPropagation();
            void form.handleSubmit();
          }}
        >
          <section className="settings-section" aria-labelledby="mode-heading">
            <div className="settings-section-copy">
              <h2 id="mode-heading">Start in</h2>
              <span>{sessionModeLabel(newSessionMode)}</span>
            </div>
            <form.Field name="mode">
              {(field) => (
                <div className="settings-segmented" role="radiogroup">
                  {settingsSessionModeOptions.map((option) => {
                    const selected = field.state.value === option.value;
                    return (
                      <label
                        key={option.value}
                        className="settings-segment"
                        data-disabled={!connected || undefined}
                        data-selected={selected || undefined}
                      >
                        <input
                          type="radio"
                          className="settings-segment-input"
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
            className="settings-section"
            aria-labelledby="thinking-heading"
          >
            <div className="settings-section-copy">
              <h2 id="thinking-heading">Thinking</h2>
              <span>{formatThinkingLabel(modelInfo?.thinking ?? "")}</span>
            </div>
            <form.Field name="thinking">
              {(field) => (
                <select
                  className="settings-select"
                  value={field.state.value}
                  disabled={modelSelectionDisabled || thinkingLevels.length < 2}
                  onBlur={field.handleBlur}
                  onChange={(event) => {
                    const level = event.target.value;
                    field.handleChange(level);
                    onThinkingChange(level);
                  }}
                >
                  {thinkingLevels.length === 0 ? (
                    <option value="">Default</option>
                  ) : (
                    thinkingLevels.map((level) => (
                      <option key={level} value={level}>
                        {formatThinkingLabel(level)}
                      </option>
                    ))
                  )}
                </select>
              )}
            </form.Field>
          </section>

          <section className="settings-section settings-models-section">
            <div className="settings-section-copy">
              <h2>Models</h2>
              <span>
                {modelOptions.length} option
                {modelOptions.length === 1 ? "" : "s"}
              </span>
            </div>
            <div className="settings-table-tools">
              <input
                type="search"
                className="settings-model-search"
                aria-label="Search models"
                placeholder="Search models"
                value={globalFilter}
                onChange={(event) => setGlobalFilter(event.target.value)}
              />
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
                  <div className="settings-table-wrap">
                    <table className="settings-model-table">
                      <thead>
                        {table.getHeaderGroups().map((headerGroup) => (
                          <tr key={headerGroup.id}>
                            <th className="settings-model-selected" />
                            {headerGroup.headers.map((header) => (
                              <th key={header.id} colSpan={header.colSpan}>
                                {header.isPlaceholder ? null : (
                                  <button
                                    type="button"
                                    className="settings-table-sort"
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
                                  </button>
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
                              data-selected={selected || undefined}
                            >
                              <td className="settings-model-selected">
                                {selected ? "✓" : ""}
                              </td>
                              {row.getVisibleCells().map((cell) => (
                                <td
                                  key={cell.id}
                                  className={
                                    cell.column.id === "label"
                                      ? "settings-model-label-cell"
                                      : undefined
                                  }
                                >
                                  {cell.column.id === "label" ? (
                                    <button
                                      type="button"
                                      className="settings-model-button"
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
                                    </button>
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
                      <div className="settings-model-empty">
                        No matching models
                      </div>
                    ) : null}
                  </div>
                  {!field.state.meta.isValid ? (
                    <div className="settings-error" role="alert">
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
