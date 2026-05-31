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

**Tool Call**:
A model-requested action that the agent may execute against the local workspace.
_Avoid_: Command, function call

**Tool Result**:
The text returned from a tool call and fed back into the agent's conversation history.
_Avoid_: Output, response

## Relationships

- A **Session** contains many **Runs**.
- A **Run** starts from one user **Turn** and produces assistant **Turns**.
- A **Run** is executed by one or more **Agents**.
- An **Agent** may execute many **Tool Calls** during one **Run**.
- Each **Tool Call** produces exactly one **Tool Result**.

## Example dialogue

> **Dev:** "Should this event be stored as a **Turn**?"
> **Domain expert:** "Only if it becomes part of the model history; UI-only progress belongs to the **Session** event log."

## Flagged ambiguities

- "message" can mean a persisted **Turn** or a renderable event payload; use **Turn** when talking about model history.
