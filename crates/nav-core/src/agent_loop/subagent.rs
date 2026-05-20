use serde_json::Value;
use std::path::Path;
use tokio::sync::mpsc::UnboundedSender;

use super::AgentEvent;
use super::control::TurnControls;
use super::runner::{SessionBinding, run_agent_inner};
use crate::cli::Args;
use crate::context::{Catalog, ProjectContext};
use crate::guardrails::{AskForApproval, PermissionContext, SandboxPolicy, SessionAllowlist};
use crate::model::ResponsesTransport;
use crate::model::responses;
use crate::tool_registry::ToolOutcome;
use crate::tool_registry::truncate::{TruncateMode, bound};

const SUBAGENT_MAX_TURNS: usize = 8;

pub(crate) struct SubagentToolRequest<'a, 's> {
    pub transport: &'a dyn ResponsesTransport,
    pub args: &'a Args,
    pub cwd: &'a Path,
    pub skills: &'a Catalog,
    pub context: Option<&'a ProjectContext>,
    pub permissions: PermissionContext,
    pub parent_events: &'a UnboundedSender<AgentEvent>,
    pub parent_session: Option<&'a SessionBinding<'s>>,
    pub call_id: &'a str,
    pub arguments: &'a Value,
}

pub(crate) async fn run_subagent_tool(request: SubagentToolRequest<'_, '_>) -> ToolOutcome {
    let Some(task) = request
        .arguments
        .get("task")
        .and_then(Value::as_str)
        .map(str::trim)
    else {
        return error_tool_outcome("tool error: missing string input field `task`");
    };
    if task.is_empty() {
        return error_tool_outcome("tool error: `task` must not be empty");
    }

    let label = request
        .arguments
        .get("label")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    emit(
        request.parent_events,
        request.parent_session,
        AgentEvent::SubagentStarted {
            id: request.call_id.to_string(),
            label: label.clone(),
            task: task.to_string(),
        },
    );

    let mut subagent_args = request.args.clone();
    subagent_args.max_turns = subagent_args.max_turns.clamp(1, SUBAGENT_MAX_TURNS);
    subagent_args.auto_compact_token_limit = 0;

    let worker_prompt = subagent_prompt(label.as_deref(), task);
    let (worker_tx, mut worker_rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    let worker_result = Box::pin(run_agent_inner(
        request.transport,
        &subagent_args,
        request.cwd,
        &worker_prompt,
        None,
        Vec::new(),
        worker_tx,
        None,
        None,
        request.skills,
        request.context,
        subagent_permissions(request.permissions),
        TurnControls::default(),
        responses::ResponseBodyOptions::read_only(),
    ))
    .await;

    let mut worker_events = Vec::new();
    while let Some(event) = worker_rx.recv().await {
        worker_events.push(event);
    }

    match worker_result {
        Ok(()) => {
            if let Some(summary) = final_subagent_summary(&worker_events) {
                let bounded = bound(summary, TruncateMode::Head);
                emit(
                    request.parent_events,
                    request.parent_session,
                    AgentEvent::SubagentCompleted {
                        id: request.call_id.to_string(),
                        summary: bounded.clone(),
                    },
                );
                text_tool_outcome(format!(
                    "subagent {} completed:\n{}",
                    subagent_display_name(request.call_id, label.as_deref()),
                    bounded
                ))
            } else {
                let message = "subagent finished without a final assistant message".to_string();
                emit_subagent_failed(
                    request.parent_events,
                    request.parent_session,
                    request.call_id,
                    &message,
                );
                error_tool_outcome(format!("tool error: {message}"))
            }
        }
        Err(err) => {
            let message =
                final_subagent_error(&worker_events).unwrap_or_else(|| format!("{err:#}"));
            emit_subagent_failed(
                request.parent_events,
                request.parent_session,
                request.call_id,
                &message,
            );
            error_tool_outcome(format!("tool error: subagent failed: {message}"))
        }
    }
}

fn subagent_prompt(label: Option<&str>, task: &str) -> String {
    let label_line = label
        .map(|value| format!("Label: {value}\n"))
        .unwrap_or_default();
    format!(
        "You are a focused nav subagent.\n\
         {label_line}\
         Work independently on the task below. You may inspect files with \
         read_file, list_files, and code_search. You cannot edit files, run \
         shell commands, request approvals, or spawn more agents. Return a \
         concise final summary with findings, files checked, and remaining \
         uncertainty.\n\n\
         Task:\n{task}"
    )
}

fn subagent_permissions(mut permissions: PermissionContext) -> PermissionContext {
    permissions.policy = AskForApproval::Never;
    permissions.sandbox_policy = SandboxPolicy::ReadOnly;
    permissions.session_allowlist = SessionAllowlist::default();
    permissions
}

fn final_subagent_summary(events: &[AgentEvent]) -> Option<String> {
    events.iter().rev().find_map(|event| match event {
        AgentEvent::AssistantMessageDone { text } if !text.trim().is_empty() => {
            Some(text.trim().to_string())
        }
        _ => None,
    })
}

fn final_subagent_error(events: &[AgentEvent]) -> Option<String> {
    events.iter().rev().find_map(|event| match event {
        AgentEvent::Error { message } => Some(message.clone()),
        _ => None,
    })
}

fn text_tool_outcome(output: impl Into<String>) -> ToolOutcome {
    ToolOutcome {
        output: output.into(),
        is_error: false,
        blocked: None,
        aborted: false,
        mutation: None,
    }
}

fn error_tool_outcome(output: impl Into<String>) -> ToolOutcome {
    ToolOutcome {
        output: output.into(),
        is_error: true,
        blocked: None,
        aborted: false,
        mutation: None,
    }
}

fn emit_subagent_failed(
    events: &UnboundedSender<AgentEvent>,
    session: Option<&SessionBinding<'_>>,
    id: &str,
    message: &str,
) {
    emit(
        events,
        session,
        AgentEvent::SubagentFailed {
            id: id.to_string(),
            message: message.to_string(),
        },
    );
}

fn subagent_display_name(id: &str, label: Option<&str>) -> String {
    match label {
        Some(label) => format!("{label} ({id})"),
        None => id.to_string(),
    }
}

fn emit(
    events: &UnboundedSender<AgentEvent>,
    session: Option<&SessionBinding<'_>>,
    event: AgentEvent,
) {
    if let Some(binding) = session
        && event.is_durable()
        && let Err(err) = binding.store.append_event(&binding.session_id, &event)
    {
        eprintln!("nav-core: failed to persist subagent event: {err:#}");
    }
    let _ = events.send(event);
}
