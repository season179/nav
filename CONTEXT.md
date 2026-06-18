# nav Context

`nav` is a local coding-agent workbench. This context names the core runtime
concepts so architecture work can stay aligned as the harness grows.

## Language

**Agent**:
The runtime actor that turns a conversation history into assistant messages by combining one model with the available tools.
_Avoid_: Bot, assistant, service

**Session**:
A resumable conversation with ordered messages and renderable events.
_Avoid_: Chat, thread

**Run**:
One execution of the agent in response to a user message. A Run may fold in
further user messages sent while it is in flight (steering), and completes only
when the model has no more work and no steering is queued.
_Avoid_: Request, job

**Steering**:
A user message sent while a Run is in flight, folded into that Run at its next
model call instead of starting a new Run.
_Avoid_: Interrupt, queue

**Turn**:
One persisted message-shaped entry in a session history.
_Avoid_: Event, log line

**Turn History**:
The ordered Turns that belong to a Session and remain the raw source of truth.
_Avoid_: Transcript, chat log

**Model Context**:
The model-visible view assembled for one Run from Turn History and, later,
other context sources.
_Avoid_: History, prompt

**Context Strategy**:
How Turn History becomes Model Context for one Run: order, ranking, pinning,
summaries, and pruning. Today's only strategy forwards every turn verbatim; the
trait is the seam future context management grows in.
_Avoid_: Prompt builder, context formatter

**Token Usage**:
The provider-reported or locally estimated token counts observed for model calls.
Used for operational visibility and context management, not billing.
_Avoid_: Cost, charge

**Token Estimate**:
A pre-call count produced from Model Context with an explicit source and
confidence, fed to a Token Budget Guard before each model call.
_Avoid_: Exact count, quota

**Context Window**:
The maximum number of tokens a model accepts in one request. Optional because
not every configured model reports one; the Token Budget Guard stays silent
when it is unknown.
_Avoid_: Token limit, max tokens

**Token Budget Guard**:
The read-only pre-call check that compares a Token Estimate against the model's
Context Window and warns (without truncating) when it nears the limit. The
measuring half of context management; pruning and compaction are the writing
half, not yet built.
_Avoid_: Compaction, truncation, limiter

**Tool**:
A model-visible capability with a schema and executor that may act against the
local workspace.
_Avoid_: Helper, command

**Tool Call**:
A model-requested invocation of a Tool.
_Avoid_: Command, function call

**Tool Result**:
The text returned from a tool call and fed back into the agent's conversation history.
_Avoid_: Output, response

## Relationships

- A **Session** contains many **Runs**.
- A **Session** owns one **Turn History**.
- A **Context Strategy** assembles one Run's **Model Context** from **Turn History**.
- A **Run** starts from one **Model Context** assembled from **Turn History**.
- A **Run** can record **Token Usage** from provider telemetry or local estimates.
- A **Token Estimate** is derived before a model call from **Model Context**.
- A **Token Budget Guard** checks a **Token Estimate** against the model's
  **Context Window** and warns when it nears the limit, without truncating.
- A **Run** starts from one user **Turn**, may fold in further user **Turns**
  sent while it runs (**Steering**), and produces assistant **Turns**.
- A **Run** is executed by one or more **Agents**.
- An **Agent** has access to many **Tools**.
- An **Agent** may execute many **Tool Calls** during one **Run**.
- Each **Tool Call** names exactly one **Tool**.
- Each **Tool Call** produces exactly one **Tool Result**.

## Example dialogue

> **Dev:** "Should this event be stored as a **Turn**?"
> **Domain expert:** "Only if it becomes part of the model history; UI-only progress belongs to the **Session** event log."

## Flagged ambiguities

- "message" can mean a persisted **Turn** or a renderable event payload; use **Turn** when talking about model history.
