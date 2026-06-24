import { resolve } from "node:path";
import { ModelCatalog } from "./model-catalog.js";
import { SessionCatalog } from "./session-catalog.js";
import { StackStore } from "./stacks.js";

export type BackendServices = {
  models: ModelCatalog;
  catalog: SessionCatalog;
  stacks: StackStore;
};

export function createBackendServices({
  dataDir = resolve(process.cwd(), "data"),
  env = process.env,
}: {
  dataDir?: string;
  env?: NodeJS.ProcessEnv;
} = {}): BackendServices {
  const models = new ModelCatalog({ env });

  return {
    models,
    catalog: new SessionCatalog({
      filePath:
        env.NAV_SESSION_CATALOG_PATH ?? resolve(dataDir, "sessions.json"),
      defaultCwd: env.NAV_AGENT_CWD ?? resolve(process.cwd(), ".."),
      worktreeBaseDir: env.NAV_WORKTREE_DIR ?? resolve(dataDir, "worktrees"),
      models,
    }),
    stacks: new StackStore({
      filePath: env.NAV_STACKS_PATH ?? resolve(dataDir, "stacks.json"),
    }),
  };
}

export const backendServices = createBackendServices();
