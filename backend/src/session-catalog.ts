import { mkdir, readFile, realpath, rename, writeFile } from "node:fs/promises";
import { basename, dirname, resolve } from "node:path";
import { createUuidV7 } from "./ids.js";
import type { ModelCatalog, ModelSelection } from "./model-catalog.js";
import type { ThinkingLevel } from "./model-types.js";
import {
  defaultExecFile,
  type ExecFileLike,
  prepareWorkspace,
  removeWorktree,
  type SessionMode,
} from "./worktrees.js";

export type SessionSummary = {
  sessionId: string;
  title: string | null;
  workspaceRoot: string | null;
  projectRoot: string | null;
  updatedAt: number;
};

export type CatalogSession = SessionSummary &
  ModelSelection & {
    mode: SessionMode;
    agentCwd: string;
    worktreePath: string | null;
    createdAt: number;
  };

type CatalogFile = {
  version: 1;
  sessions: CatalogSession[];
};

export class SessionCatalog {
  readonly #filePath: string;
  readonly #defaultCwd: string;
  readonly #worktreeBaseDir: string;
  readonly #models: ModelCatalog;
  readonly #clock: () => number;
  readonly #idFactory: () => string;
  readonly #execFile: ExecFileLike;
  #mutationQueue: Promise<unknown> = Promise.resolve();

  constructor({
    filePath,
    defaultCwd,
    worktreeBaseDir,
    models,
    clock = Date.now,
    idFactory = createUuidV7,
    execFile = defaultExecFile,
  }: {
    filePath: string;
    defaultCwd: string;
    worktreeBaseDir: string;
    models: ModelCatalog;
    clock?: () => number;
    idFactory?: () => string;
    execFile?: ExecFileLike;
  }) {
    this.#filePath = filePath;
    this.#defaultCwd = defaultCwd;
    this.#worktreeBaseDir = worktreeBaseDir;
    this.#models = models;
    this.#clock = clock;
    this.#idFactory = idFactory;
    this.#execFile = execFile;
  }

  async create({
    cwd,
    mode = "local",
  }: {
    cwd?: string | null;
    mode?: SessionMode | null;
  } = {}): Promise<CatalogSession> {
    const sessionId = this.#idFactory();
    const selectedMode = mode === "worktree" ? "worktree" : "local";
    const workspace = await prepareWorkspace({
      cwd: resolve(cwd ?? this.#defaultCwd),
      mode: selectedMode,
      sessionId,
      worktreeBaseDir: this.#worktreeBaseDir,
      execFile: this.#execFile,
    });
    const now = this.#clock();
    const defaultModel = this.#models.defaultSelection();
    const session: CatalogSession = {
      sessionId,
      title: basename(workspace.workspaceRoot) || null,
      workspaceRoot: workspace.workspaceRoot,
      projectRoot: workspace.projectRoot,
      updatedAt: now,
      createdAt: now,
      mode: selectedMode,
      agentCwd: workspace.agentCwd,
      worktreePath: workspace.worktreePath,
      ...defaultModel,
    };
    await this.mutate(async () => {
      const data = await this.load();
      data.sessions = [session, ...data.sessions];
      await this.save(data);
    });
    return session;
  }

  async list(): Promise<SessionSummary[]> {
    await this.waitForPendingMutation();
    const data = await this.load();
    return data.sessions
      .toSorted((left, right) => right.updatedAt - left.updatedAt)
      .map(toSummary);
  }

  async get(sessionId: string): Promise<CatalogSession | null> {
    await this.waitForPendingMutation();
    const data = await this.load();
    return (
      data.sessions.find((session) => session.sessionId === sessionId) ?? null
    );
  }

  async latestByCwd(cwd?: string | null): Promise<string | null> {
    await this.waitForPendingMutation();
    const data = await this.load();
    const sessions = data.sessions.toSorted(
      (left, right) => right.updatedAt - left.updatedAt,
    );

    if (!cwd) {
      return sessions[0]?.sessionId ?? null;
    }

    const resolvedCwd = await realpath(cwd).catch(() => resolve(cwd));
    const match = sessions.find(
      (session) =>
        session.workspaceRoot === resolvedCwd ||
        session.projectRoot === resolvedCwd ||
        session.agentCwd === resolvedCwd,
    );

    return match?.sessionId ?? null;
  }

  async resume(sessionId: string): Promise<CatalogSession | null> {
    return this.mutate(async () => {
      const data = await this.load();
      const session = data.sessions.find(
        (entry) => entry.sessionId === sessionId,
      );
      if (!session) {
        return null;
      }

      session.updatedAt = this.#clock();
      await this.save(data);
      return session;
    });
  }

  async updateModel(
    sessionId: string,
    selection: ModelSelection,
  ): Promise<CatalogSession | null> {
    return this.mutate(async () => {
      const data = await this.load();
      const session = data.sessions.find(
        (entry) => entry.sessionId === sessionId,
      );
      if (!session) {
        return null;
      }

      session.provider = selection.provider;
      session.model = selection.model;
      session.thinkingLevel = selection.thinkingLevel;
      session.updatedAt = this.#clock();
      await this.save(data);
      return session;
    });
  }

  async updateThinking(
    sessionId: string,
    thinkingLevel: ThinkingLevel,
  ): Promise<CatalogSession | null> {
    return this.mutate(async () => {
      const data = await this.load();
      const session = data.sessions.find(
        (entry) => entry.sessionId === sessionId,
      );
      if (!session) {
        return null;
      }

      session.thinkingLevel = thinkingLevel;
      session.updatedAt = this.#clock();
      await this.save(data);
      return session;
    });
  }

  async delete(sessionId: string): Promise<boolean> {
    const deletedSession = await this.mutate(async () => {
      const data = await this.load();
      const session = data.sessions.find(
        (entry) => entry.sessionId === sessionId,
      );
      if (!session) {
        return null;
      }

      data.sessions = data.sessions.filter(
        (entry) => entry.sessionId !== sessionId,
      );
      await this.save(data);
      return session;
    });

    if (!deletedSession) {
      return false;
    }

    if (deletedSession.worktreePath) {
      await removeWorktree({
        projectRoot:
          deletedSession.projectRoot ??
          deletedSession.workspaceRoot ??
          deletedSession.agentCwd,
        worktreePath: deletedSession.worktreePath,
        execFile: this.#execFile,
      });
    }

    return true;
  }

  private mutate<T>(run: () => Promise<T>): Promise<T> {
    const next = this.#mutationQueue.then(run, run);
    this.#mutationQueue = next.catch(() => {});
    return next;
  }

  private async waitForPendingMutation(): Promise<void> {
    await this.#mutationQueue.catch(() => {});
  }

  private async load(): Promise<CatalogFile> {
    try {
      const raw = await readFile(this.#filePath, "utf8");
      const parsed = JSON.parse(raw) as Partial<CatalogFile>;

      return {
        version: 1,
        sessions: Array.isArray(parsed.sessions) ? parsed.sessions : [],
      };
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") {
        return { version: 1, sessions: [] };
      }

      throw error;
    }
  }

  private async save(data: CatalogFile): Promise<void> {
    await mkdir(dirname(this.#filePath), { recursive: true });
    const tempPath = `${this.#filePath}.${process.pid}.${createUuidV7()}.tmp`;
    await writeFile(tempPath, `${JSON.stringify(data, null, 2)}\n`);
    await rename(tempPath, this.#filePath);
  }
}

function toSummary(session: CatalogSession): SessionSummary {
  return {
    sessionId: session.sessionId,
    title: session.title,
    workspaceRoot: session.workspaceRoot,
    projectRoot: session.projectRoot,
    updatedAt: session.updatedAt,
  };
}
