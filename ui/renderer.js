const composer = document.querySelector(".composer");
const prompt = document.querySelector("#prompt");
const sendButton = document.querySelector(".send-button");
const heading = document.querySelector("h1");
const workspaceNames = document.querySelectorAll(".workspace-name");
const projectName = document.querySelector(".project-name");
const projectToggle = document.querySelector(".project-toggle");
const sessionList = document.querySelector(".session-list");
const workspaceStripButton = document.querySelector(".workspace-strip-button");

let currentWorkspace = null;

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
    prompt.disabled = false;
    prompt.placeholder = "Ask nav anything. @ to mention files";
  }

  if (sendButton) {
    sendButton.disabled = false;
  }
};

const chooseWorkspace = async () => {
  if (!window.navApp?.selectWorkspace) {
    return;
  }

  const workspace = await window.navApp.selectWorkspace();

  if (workspace) {
    setWorkspace(workspace);
    prompt?.focus();
  }
};

const loadWorkspace = async () => {
  if (!window.navApp?.getWorkspace) {
    setWorkspace(null);
    return;
  }

  setWorkspace(await window.navApp.getWorkspace());
};

composer?.addEventListener("submit", (event) => {
  event.preventDefault();

  if (!currentWorkspace) {
    chooseWorkspace();
    return;
  }

  prompt?.focus();
});

projectToggle?.addEventListener("click", () => {
  const isExpanded = projectToggle.getAttribute("aria-expanded") !== "false";
  projectToggle.setAttribute("aria-expanded", String(!isExpanded));
  sessionList?.toggleAttribute("hidden", isExpanded);
});

workspaceStripButton?.addEventListener("click", chooseWorkspace);

loadWorkspace();
