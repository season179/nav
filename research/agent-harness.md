# Agent Harness
## 1. Models

The harness needs explicit model routing. Different work may require different models for planning, coding, summarizing, judging, cheap transforms, or long-context reasoning. This layer should define provider config, fallback behavior, cost and latency tradeoffs, and how model choices show up in evals.

## 2. Agents

Agents are the behavioral layer. They define roles, loops, task state, delegation, autonomy limits, handoff rules, and stop conditions. Models supply capability; agents decide how that capability is organized into work.

## 3. Context Management

Context management is where much of the harness quality lives. It covers what gets loaded, ranked, pinned, remembered, compressed, cited, refreshed, or discarded. Weak context management can make a strong model behave like it does not understand the task.

## 4. Tools

Tools should be explicit, typed, permissioned, observable, and recoverable. This includes schemas, auth, sandboxing, approval flows, error handling, and audit trails. Tools are not just capabilities; they are trust boundaries.

## 5. Guardrails

Guardrails define what the harness is allowed to see, do, and emit. This includes permissions, policy, sandboxing, injection resistance, destructive-action checks, data leakage prevention, role boundaries, and fail-closed behavior.

## 6. Verification

Verification proves that the agent actually did the work. It should be separate from guardrails: guardrails constrain behavior, while verification gathers evidence. Tests, evals, screenshots, diffs, static checks, runtime probes, acceptance criteria, and human review gates all live here.

## 7. Skills

Skills are reusable capability packages: procedural knowledge, domain context, scripts, references, templates, and other resources that can be loaded on demand. The harness should define how skills are discovered, described, trusted, activated, versioned, permissioned, and preserved in context. Good skills support lets agents gain specialized workflows without bloating the base prompt.

## 8. Observability

Observability makes the harness debuggable and improvable. Each run should leave a useful trace: prompts, model choices, token and cost data, latency, tool calls, approvals, context sources, memory hits, retries, errors, judgments, and final outcomes. Good traces make failures explainable, behavior auditable, and successful runs repeatable.
