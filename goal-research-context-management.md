Research how the project at `x` does context management. Treat `x` as
an absolute path; start from its README/AGENTS/CLAUDE files and the
agent-loop entry point.

Trace one full turn end-to-end, then cover:
- per-turn request assembly (system prompt, tool defs, message
  history, injected context, prompt-cache breakpoints)
- compaction: trigger, algorithm, what's preserved vs dropped, where
  the summary lands
- memory: in-session state and any cross-session persistence
- token budgeting: tokenizer, per-turn split, backpressure
- subagents: isolation, inheritance, reintegration (if present)

For every claim cite `path:line` and quote ≤10 lines. If a subsystem
doesn't exist, say so — don't invent. Call out anything non-obvious
or under-developed.
