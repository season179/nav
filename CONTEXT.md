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
One execution of the agent in response to a user message.
_Avoid_: Request, job

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

**Token Usage**:
The provider-reported or locally estimated token counts observed for model calls.
Used for operational visibility and context management, not billing.
_Avoid_: Cost, charge

**Token Estimate**:
A pre-call count produced from Model Context with an explicit source and
confidence, used as the future foundation for budget checks.
_Avoid_: Exact count, quota

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
- A **Run** starts from one **Model Context** assembled from **Turn History**.
- A **Run** can record **Token Usage** from provider telemetry or local estimates.
- A **Token Estimate** is derived before a model call from **Model Context**.
- A **Run** starts from one user **Turn** and produces assistant **Turns**.
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
