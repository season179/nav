import { randomBytes } from "node:crypto";
import { defineTool } from "@flue/runtime";
import * as v from "valibot";
import type { GitContext } from "./git-context.js";
import { agentWorktreePath } from "./worktrees.js";

const FLEET = ["glm", "deepseek-pro", "deepseek-flash"] as const;

export type FleetAgent = (typeof FLEET)[number];

type AgentPromptResponse = {
  result?:
    | string
    | {
        text?: string;
      };
};

export function createDelegationId(): string {
  const timestamp = Date.now().toString(16).padStart(12, "0");
  const randA = randomBytes(2).readUInt16BE() & 0x0fff;
  const randB = randomBytes(8);
  const firstRandB = randB[0] ?? 0;

  randB[0] = (firstRandB & 0x3f) | 0x80;

  return [
    timestamp.slice(0, 8),
    timestamp.slice(8),
    `7${randA.toString(16).padStart(3, "0")}`,
    randB.subarray(0, 2).toString("hex"),
    randB.subarray(2).toString("hex"),
  ].join("-");
}

function resultText(result: AgentPromptResponse["result"]): string {
  if (typeof result === "string") {
    return result;
  }

  return result?.text ?? "";
}

export type ConsultAgentResult = {
  agent: FleetAgent;
  answer: string;
  delegationId: string;
  worktree: string;
};

export async function consultAgent(
  gitCtx: GitContext,
  agent: FleetAgent,
  message: string,
  signal?: AbortSignal,
): Promise<ConsultAgentResult> {
  return consultAgentWithId(
    gitCtx,
    agent,
    createDelegationId(),
    message,
    signal,
  );
}

export async function consultAgentWithId(
  gitCtx: GitContext,
  agent: FleetAgent,
  id: string,
  message: string,
  signal?: AbortSignal,
): Promise<ConsultAgentResult> {
  const port = process.env.NAV_FLUE_PORT;
  const token = process.env.NAV_DESKTOP_TOKEN;

  if (!port || !token) {
    throw new Error("NAV_FLUE_PORT / NAV_DESKTOP_TOKEN not set");
  }

  const res = await fetch(
    `http://127.0.0.1:${port}/api/agents/${agent}/${id}?wait=result`,
    {
      method: "POST",
      headers: {
        "content-type": "application/json",
        authorization: `Bearer ${token}`,
        "X-Nav-Repo-Root": gitCtx.gitRoot,
        "X-Nav-Subpath": gitCtx.subpath,
      },
      body: JSON.stringify({ message }),
      signal,
    },
  );

  if (!res.ok) {
    throw new Error(
      `consult ${agent} failed: ${res.status} ${await res.text()}`,
    );
  }

  const json = (await res.json()) as AgentPromptResponse;

  return {
    agent,
    answer: resultText(json.result),
    delegationId: id,
    worktree: agentWorktreePath(agent, id, gitCtx.gitRoot),
  };
}

const toToolResult = ({ agent, answer, worktree }: ConsultAgentResult) => ({
  agent,
  answer,
  worktree,
});

export const makeConsult = (gitCtx: GitContext) =>
  defineTool({
    name: "consult",
    description:
      "Delegate a task to one engineer (glm | deepseek-pro | deepseek-flash). It works in its own checkout and returns its solution plus the worktree path. Inspect its real changes with git -C <worktree> diff.",
    input: v.object({ agent: v.picklist(FLEET), task: v.string() }),
    output: v.object({
      agent: v.string(),
      answer: v.string(),
      worktree: v.string(),
    }),
    async run({ input, signal }) {
      return toToolResult(
        await consultAgent(gitCtx, input.agent, input.task, signal),
      );
    },
  });

export const makeConsultPanel = (gitCtx: GitContext) =>
  defineTool({
    name: "consult_panel",
    description:
      "Delegate the same task to several engineers in parallel. Returns each result's answer and worktree path so Nav can compare real diffs and synthesize the final change.",
    input: v.object({ agents: v.array(v.picklist(FLEET)), task: v.string() }),
    output: v.object({
      results: v.array(
        v.object({
          agent: v.string(),
          answer: v.string(),
          worktree: v.string(),
        }),
      ),
    }),
    async run({ input, signal }) {
      if (input.agents.length === 0) {
        throw new Error("consult_panel: at least one agent is required");
      }

      if (new Set(input.agents).size !== input.agents.length) {
        throw new Error("consult_panel: duplicate agents are not allowed");
      }

      const results = await Promise.all(
        input.agents.map((agent) =>
          consultAgent(gitCtx, agent, input.task, signal),
        ),
      );

      return { results: results.map(toToolResult) };
    },
  });
