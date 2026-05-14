use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, ValueEnum};
use futures_util::StreamExt;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    env, fs,
    path::{Component, Path, PathBuf},
    process::Stdio,
};
use tokio::{process::Command, time};

// Reading guide:
// 1. Start at main() to see the whole agent loop in one place.
// 2. Read load_auth() to understand how ChatGPT/Codex subscription auth works.
// 3. Read create_response() and decode_sse_response() to see the Responses API call.
// 4. Read tool_definitions(), then run_tool() and the tool functions at the bottom.
// 5. Come back to function_calls() to see how model tool requests become Rust calls.

// clap turns this struct into the CLI. Keeping options small makes the
// educational path clear: model choice, auth choice, loop limit, and prompt.
#[derive(Parser, Debug)]
#[command(about = "A tiny Rust coding agent using the Responses API")]
struct Args {
    /// Model to use.
    #[arg(default_value = "gpt-5.5", long)]
    model: String,

    /// Authentication mode. ChatGPT reads ~/.codex/auth.json and calls the Codex Responses backend.
    #[arg(long, value_enum, default_value_t = AuthMode::Chatgpt)]
    auth: AuthMode,

    /// Codex home used for ChatGPT auth.
    #[arg(long)]
    codex_home: Option<PathBuf>,

    /// Maximum model/tool loop iterations.
    #[arg(default_value_t = 8, long)]
    max_turns: usize,

    /// Timeout for shell commands run by the bash tool.
    #[arg(default_value_t = 20, long)]
    bash_timeout_secs: u64,

    prompt: Vec<String>,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum AuthMode {
    Chatgpt,
    ApiKey,
}

// the rest of the code only needs two facts after auth is resolved:
// which endpoint to call and which bearer token to attach.
#[derive(Debug)]
struct AuthConfig {
    base_url: String,
    bearer: String,
}

// Codex's ChatGPT login stores OAuth credentials in ~/.codex/auth.json.
// We model only the fields this demo needs instead of the whole file.
#[derive(Debug, Deserialize)]
struct CodexAuthFile {
    auth_mode: Option<String>,
    tokens: Option<CodexTokens>,
}

#[derive(Debug, Deserialize)]
struct CodexTokens {
    access_token: String,
}

// for store=false streaming, the final response object may omit output.
// raw_output stores streamed output items so the next turn can replay the
// model's function_call items alongside our function_call_output items.
#[derive(Debug, Deserialize)]
struct ResponseEnvelope {
    output: Option<Vec<ResponseItem>>,
    #[serde(default, skip)]
    raw_output: Vec<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ResponseItem {
    #[serde(rename = "message")]
    Message { content: Option<Vec<MessagePart>> },
    #[serde(rename = "function_call")]
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum MessagePart {
    #[serde(rename = "output_text")]
    OutputText { text: String },
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(other)]
    Other,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if args.prompt.is_empty() {
        bail!("provide a prompt, for example: cargo run -- \"list the files\"");
    }

    let cwd = env::current_dir().context("failed to read current directory")?;
    let auth = load_auth(&args)?;
    let client = reqwest::Client::builder()
        .default_headers(default_headers(&auth)?)
        .build()
        .context("failed to build HTTP client")?;

    // Responses input is a transcript. The first item is the user prompt;
    // later turns append model tool-call items and local tool results.
    let mut input = vec![json!({
        "type": "message",
        "role": "user",
        "content": args.prompt.join(" ")
    })];

    // this is the entire "agent" idea: ask the model, run any tools it
    // requested, append the tool results, then ask again until it answers.
    for _ in 0..args.max_turns {
        let response = create_response(&client, &auth, &args, &cwd, &input).await?;
        let output = response
            .output
            .ok_or_else(|| anyhow!("Responses API returned no output"))?;

        print_messages(&output);
        let calls = function_calls(&output)?;
        if calls.is_empty() {
            return Ok(());
        }

        // with stateless store=false calls, the API does not remember the
        // previous function_call. We include it again so the matching
        // function_call_output has context and the call_id is valid.
        input.extend(response.raw_output);
        for call in calls {
            eprintln!("tool: {}({})", call.name, call.arguments);
            let result = run_tool(&cwd, args.bash_timeout_secs, &call.name, call.arguments).await;
            input.push(json!({
                "type": "function_call_output",
                "call_id": call.call_id,
                "output": match result {
                    Ok(value) => value,
                    Err(err) => format!("tool error: {err:#}"),
                }
            }));
        }
    }

    bail!("stopped after {} tool turns", args.max_turns)
}

fn load_auth(args: &Args) -> Result<AuthConfig> {
    match args.auth {
        AuthMode::ApiKey => {
            // API-key mode uses the public OpenAI API endpoint.
            let key = env::var("OPENAI_API_KEY").context("OPENAI_API_KEY is not set")?;
            Ok(AuthConfig {
                base_url: "https://api.openai.com/v1".to_string(),
                bearer: key,
            })
        }
        AuthMode::Chatgpt => {
            // ChatGPT subscription auth is not the same as OPENAI_API_KEY.
            // Codex stores an OAuth access token locally; the Codex backend
            // accepts that token at chatgpt.com/backend-api/codex.
            let codex_home = args
                .codex_home
                .clone()
                .or_else(|| env::var_os("CODEX_HOME").map(PathBuf::from))
                .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
                .context("could not determine CODEX_HOME or HOME")?;
            let auth_path = codex_home.join("auth.json");
            let raw = fs::read_to_string(&auth_path)
                .with_context(|| format!("failed to read {}", auth_path.display()))?;
            let auth_file: CodexAuthFile =
                serde_json::from_str(&raw).context("failed to parse Codex auth.json")?;
            if auth_file.auth_mode.as_deref() != Some("chatgpt") {
                bail!(
                    "{} is not in ChatGPT auth mode; run `codex login` and choose Sign in with ChatGPT",
                    auth_path.display()
                );
            }
            let bearer = auth_file
                .tokens
                .map(|tokens| tokens.access_token)
                .context("Codex auth.json does not contain an access token")?;
            Ok(AuthConfig {
                base_url: "https://chatgpt.com/backend-api/codex".to_string(),
                bearer,
            })
        }
    }
}

fn default_headers(auth: &AuthConfig) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", auth.bearer))?,
    );
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    // the Codex backend requires streaming, so the client asks for SSE.
    headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
    Ok(headers)
}

async fn create_response(
    client: &reqwest::Client,
    auth: &AuthConfig,
    args: &Args,
    cwd: &Path,
    input: &[Value],
) -> Result<ResponseEnvelope> {
    // tools are just JSON descriptions. The model decides whether to emit
    // a function_call item; Rust remains responsible for actually doing work.
    let body = json!({
        "model": args.model,
        "instructions": format!(
            "You are a small coding agent running in {}. Use tools to inspect, edit, search, and verify code. Prefer small, explicit steps. Paths must be relative.",
            cwd.display()
        ),
        "input": input,
        // ChatGPT/Codex subscription calls require store=false and stream=true.
        // store=false also makes the transcript handling explicit for learning.
        "store": false,
        "stream": true,
        "tools": tool_definitions(),
    });

    let response = client
        .post(format!("{}/responses", auth.base_url))
        .json(&body)
        .send()
        .await
        .context("Responses API request failed")?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        bail!("Responses API returned {status}: {body}");
    }

    decode_sse_response(response).await
}

async fn decode_sse_response(response: reqwest::Response) -> Result<ResponseEnvelope> {
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut completed = None;
    let mut output = Vec::new();
    let mut raw_output = Vec::new();

    // SSE arrives as arbitrary byte chunks, not neat JSON objects. We keep
    // a string buffer and split only when a full "\n\n"-terminated frame arrives.
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("failed to read Responses API stream")?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(index) = buffer.find("\n\n") {
            let frame = buffer[..index].to_string();
            buffer.drain(..index + 2);

            for line in frame.lines() {
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data.is_empty() || data == "[DONE]" {
                    continue;
                }

                let event: Value =
                    serde_json::from_str(data).context("failed to decode SSE event")?;
                if event.get("type").and_then(Value::as_str) == Some("response.completed") {
                    // completion tells us the stream is done and carries
                    // usage/status metadata. In this backend shape, output may
                    // still be empty, so output_item.done events matter too.
                    let response = event
                        .get("response")
                        .cloned()
                        .context("response.completed event had no response")?;
                    let mut envelope = serde_json::from_value::<ResponseEnvelope>(response.clone())
                        .context("failed to decode completed response")?;
                    envelope.raw_output = response
                        .get("output")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    completed = Some(envelope);
                } else if event.get("type").and_then(Value::as_str)
                    == Some("response.output_item.done")
                {
                    // completed output items are the durable transcript
                    // units: messages for humans and function_call items for tools.
                    let item = event
                        .get("item")
                        .cloned()
                        .context("response.output_item.done event had no item")?;
                    raw_output.push(item.clone());
                    output.push(
                        serde_json::from_value::<ResponseItem>(item)
                            .context("failed to decode output item")?,
                    );
                }
            }
        }
    }

    let mut completed =
        completed.context("Responses API stream ended without response.completed")?;
    if completed.output.as_ref().is_none_or(Vec::is_empty) {
        completed.output = Some(output);
    }
    if completed.raw_output.is_empty() {
        completed.raw_output = raw_output;
    }
    Ok(completed)
}

fn tool_definitions() -> Vec<Value> {
    // these five primitives mirror the workshop article. Together they let
    // the model inspect code, find code, change code, and verify with commands.
    vec![
        json!({
            "type": "function",
            "name": "read_file",
            "description": "Read the contents of a relative file path. Do not use this with directories.",
            "parameters": {
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"],
                "additionalProperties": false
            }
        }),
        json!({
            "type": "function",
            "name": "list_files",
            "description": "List files and directories at a relative path. Use '.' for the current directory.",
            "parameters": {
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"],
                "additionalProperties": false
            }
        }),
        json!({
            "type": "function",
            "name": "bash",
            "description": "Execute a shell command and return stdout/stderr. Use for builds, tests, and small checks.",
            "parameters": {
                "type": "object",
                "properties": { "command": { "type": "string" } },
                "required": ["command"],
                "additionalProperties": false
            }
        }),
        json!({
            "type": "function",
            "name": "edit_file",
            "description": "Create a file when old_str is empty, or replace one exact old_str occurrence with new_str.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old_str": { "type": "string" },
                    "new_str": { "type": "string" }
                },
                "required": ["path", "old_str", "new_str"],
                "additionalProperties": false
            }
        }),
        json!({
            "type": "function",
            "name": "code_search",
            "description": "Search source text for a pattern, like ripgrep.",
            "parameters": {
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string" }
                },
                "required": ["pattern", "path"],
                "additionalProperties": false
            }
        }),
    ]
}

#[derive(Debug)]
struct ToolCall {
    call_id: String,
    name: String,
    arguments: Value,
}

fn function_calls(output: &[ResponseItem]) -> Result<Vec<ToolCall>> {
    // function_call arguments arrive as a JSON string. Parsing here gives
    // each local tool strongly shaped input before it touches the filesystem.
    output
        .iter()
        .filter_map(|item| match item {
            ResponseItem::FunctionCall {
                call_id,
                name,
                arguments,
            } => Some((call_id, name, arguments)),
            _ => None,
        })
        .map(|(call_id, name, arguments)| {
            Ok(ToolCall {
                call_id: call_id.clone(),
                name: name.clone(),
                arguments: serde_json::from_str(arguments)
                    .with_context(|| format!("failed to parse arguments for {name}"))?,
            })
        })
        .collect()
}

fn print_messages(output: &[ResponseItem]) {
    // only message items are user-facing. Reasoning and tool-call items are
    // part of the loop, but printing them would make the CLI noisy.
    for item in output {
        if let ResponseItem::Message {
            content: Some(parts),
        } = item
        {
            for part in parts {
                match part {
                    MessagePart::OutputText { text } | MessagePart::Text { text } => {
                        println!("{text}")
                    }
                    MessagePart::Other => {}
                }
            }
        }
    }
}

async fn run_tool(cwd: &Path, timeout_secs: u64, name: &str, input: Value) -> Result<String> {
    // central dispatch keeps the trust boundary obvious. The model asks;
    // this Rust match decides exactly which local capability is allowed.
    match name {
        "read_file" => read_file(cwd, string_arg(&input, "path")?),
        "list_files" => list_files(cwd, string_arg(&input, "path")?),
        "bash" => bash(cwd, timeout_secs, string_arg(&input, "command")?).await,
        "edit_file" => edit_file(
            cwd,
            string_arg(&input, "path")?,
            string_arg(&input, "old_str")?,
            string_arg(&input, "new_str")?,
        ),
        "code_search" => {
            code_search(
                cwd,
                string_arg(&input, "pattern")?,
                string_arg(&input, "path")?,
            )
            .await
        }
        other => Err(anyhow!("unknown tool: {other}")),
    }
}

fn string_arg<'a>(input: &'a Value, key: &str) -> Result<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing string input field `{key}`"))
}

fn resolve_inside(root: &Path, requested: &str) -> Result<PathBuf> {
    // coding agents should not freely read or edit the whole machine. This
    // demo restricts path tools to relative paths under the current workspace.
    let path = Path::new(requested);
    if path.is_absolute() {
        bail!("absolute paths are not allowed");
    }
    if path
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        bail!("parent directory traversal is not allowed");
    }
    Ok(root.join(path))
}

fn read_file(cwd: &Path, path: &str) -> Result<String> {
    let path = resolve_inside(cwd, path)?;
    if path.is_dir() {
        bail!("{} is a directory", path.display());
    }
    fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))
}

fn list_files(cwd: &Path, path: &str) -> Result<String> {
    let path = resolve_inside(cwd, path)?;
    let mut entries = fs::read_dir(&path)
        .with_context(|| format!("failed to list {}", path.display()))?
        .map(|entry| {
            let entry = entry?;
            let mut name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type()?.is_dir() {
                name.push('/');
            }
            Ok(name)
        })
        .collect::<Result<Vec<_>>>()?;
    entries.sort();
    Ok(serde_json::to_string_pretty(&entries)?)
}

async fn bash(cwd: &Path, timeout_secs: u64, command: &str) -> Result<String> {
    // shell access is powerful and risky. We run in the workspace, capture
    // stdout/stderr for the model, and enforce a timeout so commands cannot hang.
    let child = Command::new("sh")
        .arg("-lc")
        .arg(command)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn command `{command}`"))?;

    let output = match time::timeout(
        time::Duration::from_secs(timeout_secs),
        child.wait_with_output(),
    )
    .await
    {
        Ok(output) => output?,
        Err(_) => bail!("command timed out after {timeout_secs}s: {command}"),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    Ok(format!(
        "status: {}\nstdout:\n{}\nstderr:\n{}",
        output.status, stdout, stderr
    ))
}

fn edit_file(cwd: &Path, path: &str, old_str: &str, new_str: &str) -> Result<String> {
    let path = resolve_inside(cwd, path)?;
    if old_str.is_empty() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, new_str)?;
        return Ok(format!("created {}", path.display()));
    }

    let original =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    // exact replacement is safer for a teaching agent than fuzzy patching.
    // Requiring one match prevents accidental broad edits.
    let matches = original.matches(old_str).count();
    if matches != 1 {
        bail!("expected exactly one match for old_str, found {matches}");
    }
    let updated = original.replacen(old_str, new_str, 1);
    fs::write(&path, updated)?;
    Ok(format!("edited {}", path.display()))
}

async fn code_search(cwd: &Path, pattern: &str, path: &str) -> Result<String> {
    let path = resolve_inside(cwd, path)?;
    let output = Command::new("rg")
        .arg("--line-number")
        .arg("--no-heading")
        .arg(pattern)
        .arg(&path)
        .current_dir(cwd)
        .output()
        .await
        .context("failed to run rg")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.success() || output.status.code() == Some(1) {
        Ok(stdout.into_owned())
    } else {
        Ok(format!("rg failed: {stderr}"))
    }
}
