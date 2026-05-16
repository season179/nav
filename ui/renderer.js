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
