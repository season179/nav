import {
  createUIMessageStream,
  createUIMessageStreamResponse,
  type UIMessageStreamWriter,
} from "ai";

import type { AgentRunner, AgentStreamEvent } from "./agent-runner.js";

const TEXT_PART_ID = "nav-response";

const toJson = (value: unknown): string => {
  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
};

const writeToolText = (
  writer: UIMessageStreamWriter,
  event: AgentStreamEvent,
) => {
  if (event.type === "tool-call") {
    writer.write({
      delta: `\n\nRunning ${event.toolName}...\n`,
      id: TEXT_PART_ID,
      type: "text-delta",
    });
    writer.write({
      input: event.input,
      toolCallId: event.toolCallId,
      toolName: event.toolName,
      type: "tool-input-available",
    });
    return;
  }

  if (event.type === "tool-result") {
    if (event.isError) {
      writer.write({
        errorText: toJson(event.output),
        toolCallId: event.toolCallId,
        type: "tool-output-error",
      });
      return;
    }

    writer.write({
      output: event.output,
      toolCallId: event.toolCallId,
      type: "tool-output-available",
    });
  }
};

export const createChatStreamResponse = (
  runner: AgentRunner,
  prompt: string,
  signal?: AbortSignal,
): Response => {
  let textStarted = false;

  const stream = createUIMessageStream({
    execute: async ({ writer }) => {
      const ensureTextStarted = () => {
        if (textStarted) {
          return;
        }

        writer.write({ id: TEXT_PART_ID, type: "text-start" });
        textStarted = true;
      };

      for await (const event of runner.run(prompt, { signal })) {
        if (event.type === "text-delta") {
          ensureTextStarted();
          writer.write({
            delta: event.delta,
            id: TEXT_PART_ID,
            type: "text-delta",
          });
          continue;
        }

        if (event.type === "error") {
          ensureTextStarted();
          writer.write({
            delta: `\n\nNav hit an error: ${event.message}`,
            id: TEXT_PART_ID,
            type: "text-delta",
          });
          writer.write({
            errorText: event.message,
            type: "error",
          });
          continue;
        }

        ensureTextStarted();
        writeToolText(writer, event);
      }

      if (!textStarted) {
        ensureTextStarted();
        writer.write({
          delta:
            "Nav finished without returning any text. Check the local agent logs for details.",
          id: TEXT_PART_ID,
          type: "text-delta",
        });
      }

      writer.write({ id: TEXT_PART_ID, type: "text-end" });
    },
    onError: (error) =>
      error instanceof Error ? error.message : toJson(error),
  });

  return createUIMessageStreamResponse({ stream });
};
