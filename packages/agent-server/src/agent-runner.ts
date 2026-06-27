import {
  AgentHarness,
  type AgentHarnessEvent,
  type AgentTool,
  InMemorySessionRepo,
  NodeExecutionEnv,
} from "@earendil-works/pi-agent-core/node";
import { Type } from "@earendil-works/pi-ai";

import { NAV_WORKSPACE_CWD, resolvePiModel } from "./config.js";
import { AsyncQueue } from "./queue.js";

export type AgentStreamEvent =
  | { type: "text-delta"; delta: string }
  | {
      type: "tool-call";
      toolCallId: string;
      toolName: string;
      input: unknown;
    }
  | {
      type: "tool-result";
      toolCallId: string;
      toolName: string;
      output: unknown;
      isError: boolean;
    }
  | { type: "error"; message: string };

export interface AgentRunner {
  run(
    prompt: string,
    options?: { signal?: AbortSignal },
  ): AsyncIterable<AgentStreamEvent>;
}

const asErrorMessage = (error: unknown): string =>
  error instanceof Error ? error.message : String(error);

const createTools = (env: NodeExecutionEnv): AgentTool[] => [
  {
    description: "Read a UTF-8 text file from the current workspace.",
    execute: async (_toolCallId, params, signal) => {
      const readParams = params as { maxBytes?: number; path: string };
      const result = await env.readTextFile(readParams.path, signal);
      if (!result.ok) {
        throw result.error;
      }

      const maxBytes = readParams.maxBytes ?? 20_000;
      const text = result.value.slice(0, maxBytes);

      return {
        content: [{ text, type: "text" }],
        details: {
          bytesReturned: text.length,
          truncated: result.value.length > text.length,
        },
      };
    },
    label: "Read file",
    name: "read_file",
    parameters: Type.Object({
      maxBytes: Type.Optional(Type.Number()),
      path: Type.String(),
    }),
  },
  {
    description:
      "List direct children of a directory in the current workspace.",
    execute: async (_toolCallId, params, signal) => {
      const listParams = params as { path?: string };
      const result = await env.listDir(listParams.path ?? ".", signal);
      if (!result.ok) {
        throw result.error;
      }

      const lines = result.value
        .map((entry) => `${entry.kind}\t${entry.path}`)
        .join("\n");

      return {
        content: [{ text: lines, type: "text" }],
        details: { count: result.value.length },
      };
    },
    label: "List directory",
    name: "list_dir",
    parameters: Type.Object({
      path: Type.Optional(Type.String()),
    }),
  },
  {
    description:
      "Run a shell command in the current workspace with a short timeout.",
    execute: async (_toolCallId, params, signal) => {
      const bashParams = params as {
        command: string;
        cwd?: string;
        timeout?: number;
      };
      const timeout = Math.min(Math.max(bashParams.timeout ?? 30, 1), 60);
      const result = await env.exec(bashParams.command, {
        cwd: bashParams.cwd,
        timeout,
        abortSignal: signal,
      });
      if (!result.ok) {
        throw result.error;
      }

      const output = [
        result.value.stdout,
        result.value.stderr ? `\n[stderr]\n${result.value.stderr}` : "",
        `\n[exit ${result.value.exitCode}]`,
      ].join("");

      return {
        content: [{ text: output.slice(-20_000), type: "text" }],
        details: {
          exitCode: result.value.exitCode,
          stderrBytes: result.value.stderr.length,
          stdoutBytes: result.value.stdout.length,
        },
      };
    },
    label: "Shell",
    name: "bash",
    parameters: Type.Object({
      command: Type.String(),
      cwd: Type.Optional(Type.String()),
      timeout: Type.Optional(Type.Number()),
    }),
  },
];

const getActiveToolNames = () =>
  process.env.NAV_AGENT_ENABLE_BASH === "1"
    ? ["read_file", "list_dir", "bash"]
    : ["read_file", "list_dir"];

const mapHarnessEvent = (
  event: AgentHarnessEvent,
  queue: AsyncQueue<AgentStreamEvent>,
) => {
  if (event.type === "message_update") {
    const assistantEvent = event.assistantMessageEvent;
    if (assistantEvent.type === "text_delta") {
      queue.push({ delta: assistantEvent.delta, type: "text-delta" });
    }
    return;
  }

  if (event.type === "tool_execution_start") {
    queue.push({
      input: event.args,
      toolCallId: event.toolCallId,
      toolName: event.toolName,
      type: "tool-call",
    });
    return;
  }

  if (event.type === "tool_execution_end") {
    queue.push({
      isError: event.isError,
      output: event.result,
      toolCallId: event.toolCallId,
      toolName: event.toolName,
      type: "tool-result",
    });
  }
};

export class PiAgentRunner implements AgentRunner {
  async *run(
    prompt: string,
    options: { signal?: AbortSignal } = {},
  ): AsyncIterable<AgentStreamEvent> {
    const queue = new AsyncQueue<AgentStreamEvent>();
    const env = new NodeExecutionEnv({
      cwd: NAV_WORKSPACE_CWD,
      shellEnv: process.env,
      shellPath: process.env.SHELL ?? "/bin/zsh",
    });
    const repo = new InMemorySessionRepo();
    const session = await repo.create();
    const { model, models } = await resolvePiModel();

    const harness = new AgentHarness({
      activeToolNames: getActiveToolNames(),
      env,
      model,
      models,
      session,
      systemPrompt:
        "You are Nav, a local coding agent running inside /Users/season/Personal/nav. Keep answers concise and use tools only when they materially help.",
      tools: createTools(env),
    });

    const unsubscribe = harness.subscribe((event) =>
      mapHarnessEvent(event, queue),
    );
    const abort = () => {
      void harness.abort();
    };

    options.signal?.addEventListener("abort", abort, { once: true });

    void harness
      .prompt(prompt)
      .catch((error) => {
        queue.push({ message: asErrorMessage(error), type: "error" });
      })
      .finally(() => {
        unsubscribe();
        options.signal?.removeEventListener("abort", abort);
        queue.close();
        void env.cleanup();
      });

    for await (const event of queue) {
      yield event;
    }
  }
}

export class MockAgentRunner implements AgentRunner {
  async *run(prompt: string): AsyncIterable<AgentStreamEvent> {
    yield { delta: `Mock Nav response for: ${prompt}`, type: "text-delta" };
  }
}
