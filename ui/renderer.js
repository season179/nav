const composer = document.querySelector(".composer");
const prompt = document.querySelector("#prompt");
const sendButton = document.querySelector(".send-button");
const heading = document.querySelector("h1");
const workspaceNames = document.querySelectorAll(".workspace-name");
const projectName = document.querySelector(".project-name");
const projectToggle = document.querySelector(".project-toggle");
const sessionList = document.querySelector(".session-list");
const workspaceStripButton = document.querySelector(".workspace-strip-button");
const newSessionButton = document.querySelector(".nav-action");
const sessionOutput = document.querySelector(".session-output");

let currentWorkspace = null;
let isRunning = false;
let currentRun = null;

const scrollSessionToBottom = () => {
  if (sessionOutput) {
    sessionOutput.scrollTop = sessionOutput.scrollHeight;
  }
};

const previewText = (text, maxChars = 4000) => {
  if (!text || text.length <= maxChars) {
    return text ?? "";
  }
  return `${text.slice(0, maxChars)}\n... truncated ...`;
};

const patchPathSummary = (patch) => {
  const entries = [];
  let lastUpdateIndex = null;
  for (const line of patch.split(/\r?\n/)) {
    if (line.startsWith("*** Update File: ")) {
      lastUpdateIndex = entries.length;
      entries.push(`M ${line.slice("*** Update File: ".length)}`);
    } else if (line.startsWith("*** Move to: ")) {
      if (lastUpdateIndex !== null) {
        entries[lastUpdateIndex] += ` -> ${line.slice("*** Move to: ".length)}`;
      }
    } else if (line.startsWith("*** Add File: ")) {
      lastUpdateIndex = null;
      entries.push(`A ${line.slice("*** Add File: ".length)}`);
    } else if (line.startsWith("*** Delete File: ")) {
      lastUpdateIndex = null;
      entries.push(`D ${line.slice("*** Delete File: ".length)}`);
    }
  }

  if (entries.length === 0) {
    return "apply_patch";
  }

  const shown = entries.slice(0, 6);
  const hidden = entries.length - shown.length;
  return `apply_patch ${shown.join(", ")}${hidden > 0 ? `, ... ${hidden} more` : ""}`;
};

const toolSummary = (event) => {
  if (event.name === "apply_patch") {
    return patchPathSummary(event.arguments?.patch ?? "");
  }
  if (typeof event.arguments?.path === "string") {
    return `${event.name} ${event.arguments.path}`;
  }
  if (typeof event.arguments?.command === "string") {
    return `bash ${event.arguments.command}`;
  }
  return event.name;
};

const changePath = (change) => {
  if (change.kind === "update" && change.move_path) {
    return change.line_start
      ? `${change.path} -> ${change.move_path}:${change.line_start}`
      : `${change.path} -> ${change.move_path}`;
  }
  return change.line_start ? `${change.path}:${change.line_start}` : change.path;
};

const changeLetter = (change) => {
  if (change.kind === "add") {
    return "A";
  }
  if (change.kind === "delete") {
    return "D";
  }
  return "M";
};

const renderFileChange = (event) => {
  const lines = [event.summary ?? `${event.changes?.length ?? 0} files changed`];
  for (const change of event.changes ?? []) {
    lines.push(
      `${changeLetter(change)} ${changePath(change)} (+${change.additions ?? 0} -${change.deletions ?? 0})`,
    );
    if (change.diff) {
      lines.push(previewText(change.diff));
    }
  }
  if (event.error) {
    lines.push(event.error);
  }
  return lines.join("\n");
};

const renderTurnDiff = (diff) => {
  const files = diff.files ?? [];
  const fileWord = files.length === 1 ? "file" : "files";
  const lines = [`${files.length} ${fileWord} changed`];
  for (const file of files) {
    lines.push(
      `${file.status} ${file.path} (+${file.additions ?? 0} -${file.deletions ?? 0})`,
    );
  }
  if (diff.unified_diff) {
    lines.push(previewText(diff.unified_diff));
  }
  if (diff.truncated) {
    lines.push("full diff truncated");
  }
  return lines.join("\n");
};

const setRunning = (running) => {
  isRunning = running;

  if (prompt) {
    prompt.disabled = running;
  }

  if (sendButton) {
    sendButton.disabled = running;
  }
};

const showSession = () => {
  document.body.classList.add("session-active");

  if (sessionOutput) {
    sessionOutput.hidden = false;
  }
};

const resetSession = () => {
  if (isRunning || !sessionOutput) {
    return;
  }

  sessionOutput.replaceChildren();
  sessionOutput.hidden = true;
  document.body.classList.remove("session-active");
  currentRun = null;
  prompt?.focus();
};

const appendMessage = (kind, text) => {
  if (!sessionOutput) {
    return null;
  }

  showSession();

  const message = document.createElement("div");
  message.className = `session-message ${kind}`;
  message.textContent = text;
  sessionOutput.append(message);
  scrollSessionToBottom();
  return message;
};

const appendStreamMessage = (kind) => {
  const message = appendMessage(kind, "");

  if (message) {
    message.hidden = true;
  }

  return {
    append(text) {
      if (!message || !text) {
        return;
      }

      message.hidden = false;
      message.textContent += text;
      scrollSessionToBottom();
    },
  };
};

const setWorkspace = (workspace) => {
  currentWorkspace = workspace;
  const hasWorkspace = Boolean(workspace);
  const name = workspace?.name ?? "Select working directory";

  document.body.classList.toggle("has-workspace", hasWorkspace);

  workspaceNames.forEach((element) => {
    element.textContent = name;
  });

  if (projectName) {
    projectName.textContent = workspace?.name ?? "No project selected";
  }

  if (projectToggle) {
    projectToggle.setAttribute(
      "aria-label",
      workspace?.name ?? "No project selected",
    );
  }

  if (heading) {
    heading.textContent = hasWorkspace
      ? `What should we build in ${name}?`
      : "What should we work on?";
  }

  if (workspaceStripButton) {
    workspaceStripButton.setAttribute(
      "aria-label",
      hasWorkspace ? name : "Select working directory",
    );
  }

  if (prompt) {
    prompt.disabled = isRunning;
    prompt.placeholder = "Ask Nav anything. @ to mention files";
  }

  if (sendButton) {
    sendButton.disabled = isRunning;
  }
};

const chooseWorkspace = async () => {
  if (!window.navApp?.selectWorkspace) {
    return null;
  }

  const workspace = await window.navApp.selectWorkspace();

  if (workspace) {
    setWorkspace(workspace);
    prompt?.focus();
  }

  return workspace;
};

const loadWorkspace = async () => {
  if (!window.navApp?.getWorkspace) {
    setWorkspace(null);
    return;
  }

  setWorkspace(await window.navApp.getWorkspace());
};

const runPrompt = async (text) => {
  showSession();
  appendMessage("user", text);

  currentRun = {
    assistant: appendStreamMessage("agent"),
    assistantText: "",
    log: appendStreamMessage("log"),
  };

  if (prompt) {
    prompt.value = "";
  }

  setRunning(true);

  try {
    await window.navApp.runAgent(text);
  } catch (error) {
    appendMessage("error", error.message ?? String(error));
  } finally {
    setRunning(false);
    currentRun = null;
    prompt?.focus();
  }
};

window.navApp?.onAgentEvent?.((event) => {
  if (!event || !currentRun) {
    return;
  }

  if (event.type === "agent_event") {
    const agentEvent = event.event;
    if (!agentEvent) {
      return;
    }

    if (agentEvent.kind === "assistant_message_delta") {
      currentRun.assistantText += agentEvent.text ?? "";
      currentRun.assistant.append(agentEvent.text);
      return;
    }

    if (agentEvent.kind === "assistant_message_done") {
      if (!currentRun.assistantText) {
        currentRun.assistantText = agentEvent.text ?? "";
        currentRun.assistant.append(agentEvent.text);
      }
      return;
    }

    if (agentEvent.kind === "tool_call_started") {
      appendMessage("tool", toolSummary(agentEvent));
      return;
    }

    if (agentEvent.kind === "tool_call_output") {
      if (agentEvent.is_error) {
        appendMessage("error", agentEvent.output);
      } else if (agentEvent.output) {
        appendMessage("log", agentEvent.output);
      }
      return;
    }

    if (agentEvent.kind === "file_change") {
      appendMessage(
        agentEvent.status === "failed" ? "error" : "change",
        renderFileChange(agentEvent),
      );
      return;
    }

    if (agentEvent.kind === "turn_diff") {
      appendMessage("diff", renderTurnDiff(agentEvent));
      return;
    }

    if (agentEvent.kind === "error") {
      appendMessage("error", agentEvent.message);
    }
    return;
  }

  if (event.type === "stdout") {
    currentRun.assistant.append(event.text);
    return;
  }

  if (event.type === "stderr") {
    currentRun.log.append(event.text);
    return;
  }

  if (event.type === "error") {
    appendMessage("error", event.message);
    return;
  }

  if (event.type === "done" && !event.ok) {
    appendMessage(
      "error",
      `Nav exited with status ${event.exitCode ?? event.signal ?? "unknown"}.`,
    );
  }
});

composer?.addEventListener("submit", async (event) => {
  event.preventDefault();

  if (isRunning) {
    return;
  }

  const text = prompt?.value.trim() ?? "";
  if (!text) {
    prompt?.focus();
    return;
  }

  if (!currentWorkspace) {
    const workspace = await chooseWorkspace();
    if (!workspace) {
      return;
    }
  }

  await runPrompt(text);
});

projectToggle?.addEventListener("click", () => {
  const isExpanded = projectToggle.getAttribute("aria-expanded") !== "false";
  projectToggle.setAttribute("aria-expanded", String(!isExpanded));
  sessionList?.toggleAttribute("hidden", isExpanded);
});

workspaceStripButton?.addEventListener("click", chooseWorkspace);
newSessionButton?.addEventListener("click", resetSession);

loadWorkspace();
