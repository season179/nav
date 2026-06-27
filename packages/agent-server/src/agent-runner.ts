import { basename } from "node:path";

import { HarnessAgent } from "@ai-sdk/harness/agent";
import { createPi } from "@ai-sdk/harness-pi";
import { createJustBashSandbox } from "@ai-sdk/sandbox-just-bash";
import {
  createUIMessageStream,
  createUIMessageStreamResponse,
  type ModelMessage,
  toUIMessageStream,
} from "ai";
import { OverlayFs } from "just-bash";

import { NAV_WORKSPACE_CWD, resolvePiHarnessSettings } from "./config.js";

const TEXT_PART_ID = "nav-response";
const SANDBOX_WORKSPACE_ROOT = "/workspace";
const SANDBOX_WORKSPACE_NAME = basename(NAV_WORKSPACE_CWD);
const SANDBOX_WORKSPACE_CWD = `${SANDBOX_WORKSPACE_ROOT}/${SANDBOX_WORKSPACE_NAME}`;
const NAV_AGENT_INSTRUCTIONS =
  "You are Nav, a local coding agent running in the current workspace. Keep answers concise and use tools only when they materially help.";

export interface AgentRunner {
  createResponse(
    messages: ModelMessage[],
    options?: { signal?: AbortSignal },
  ): Promise<Response> | Response;
}

const getSandboxEnv = (): Record<string, string> =>
  Object.fromEntries(
    Object.entries(process.env).filter(
      (entry): entry is [string, string] => typeof entry[1] === "string",
    ),
  );

const createNavSandboxProvider = () =>
  createJustBashSandbox({
    cwd: SANDBOX_WORKSPACE_ROOT,
    env: getSandboxEnv(),
    fs: new OverlayFs({
      allowSymlinks: true,
      mountPoint: SANDBOX_WORKSPACE_CWD,
      root: NAV_WORKSPACE_CWD,
    }),
  });

const getTextContent = (content: ModelMessage["content"]): string => {
  if (typeof content === "string") {
    return content;
  }

  if (!Array.isArray(content)) {
    return "";
  }

  return content
    .map((part) =>
      typeof part === "object" &&
      part !== null &&
      "type" in part &&
      part.type === "text" &&
      "text" in part &&
      typeof part.text === "string"
        ? part.text
        : "",
    )
    .join("");
};

const getLatestUserText = (messages: ModelMessage[]): string => {
  const latestUserMessage = messages.findLast(
    (message) => message.role === "user",
  );

  return latestUserMessage ? getTextContent(latestUserMessage.content) : "";
};

export class PiAgentRunner implements AgentRunner {
  async createResponse(
    messages: ModelMessage[],
    options: { signal?: AbortSignal } = {},
  ): Promise<Response> {
    const { provider: _provider, ...piSettings } =
      await resolvePiHarnessSettings();
    const agent = new HarnessAgent({
      harness: createPi(piSettings),
      instructions: NAV_AGENT_INSTRUCTIONS,
      permissionMode: "allow-all",
      sandbox: createNavSandboxProvider(),
      sandboxConfig: { workDir: SANDBOX_WORKSPACE_NAME },
    });
    const session = await agent.createSession({
      abortSignal: options.signal,
    });
    let destroyStarted = false;

    const destroySession = async () => {
      if (destroyStarted) {
        return;
      }

      destroyStarted = true;
      options.signal?.removeEventListener("abort", abortSession);
      try {
        await session.destroy();
      } catch (error) {
        console.error("Failed to destroy Nav agent session.", error);
      }
    };
    const abortSession = () => {
      void destroySession();
    };

    options.signal?.addEventListener("abort", abortSession, { once: true });

    try {
      const result = await agent.stream({
        abortSignal: options.signal,
        messages,
        session,
      });
      const stream = toUIMessageStream({
        onEnd: destroySession,
        onError: (error) =>
          error instanceof Error ? error.message : String(error),
        stream: result.stream,
        tools: agent.tools,
      });

      return createUIMessageStreamResponse({ stream });
    } catch (error) {
      await destroySession();
      throw error;
    }
  }
}

export class MockAgentRunner implements AgentRunner {
  createResponse(messages: ModelMessage[]): Response {
    const text = getLatestUserText(messages);
    const stream = createUIMessageStream({
      execute: ({ writer }) => {
        writer.write({ id: TEXT_PART_ID, type: "text-start" });
        writer.write({
          delta: `Mock Nav response for: ${text}`,
          id: TEXT_PART_ID,
          type: "text-delta",
        });
        writer.write({ id: TEXT_PART_ID, type: "text-end" });
      },
    });

    return createUIMessageStreamResponse({ stream });
  }
}
