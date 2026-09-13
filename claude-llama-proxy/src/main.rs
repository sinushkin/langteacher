//! Presents itself on the wire as a llama.cpp server (`/health`, `/props`,
//! `/v1/chat/completions`), but forwards every turn to a single resident
//! `claude -p --input-format stream-json --output-format stream-json`
//! subprocess instead of a local GGUF model. The subprocess is spawned once
//! and kept alive for the life of the proxy, so conversation context lives
//! in Claude Code's own session rather than being replayed on each request.

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{
        IntoResponse, Response,
        sse::{Event, Sse},
    },
    routing::{get, post},
};
use chrono::Local;
use futures::StreamExt;
use futures::stream;
use serde::Deserialize;
use serde_json::{Value, json};
use std::convert::Infallible;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::UnboundedReceiverStream;

const LISTEN_ADDR: &str = "127.0.0.1:8080";
const PROXY_URL: &str = "http://127.0.0.1:8118";
// Cheapest/fastest model -- a live voice tutor is short back-and-forth turns,
// not the kind of task that needs the biggest model.
const CLAUDE_MODEL: &str = "haiku";
const DEFAULT_SYSTEM_PROMPT: &str = "You are a helpful assistant.";

// Blocked so the tutor persona can never wander into acting like a coding
// agent (editing files, running shell commands, etc.) mid-conversation.
const DISALLOWED_TOOLS: &str = "Bash Edit Write Read Glob Grep WebFetch WebSearch Agent \
    Skill ScheduleWakeup Monitor CronCreate CronDelete CronList DesignSync \
    EnterWorktree ExitWorktree ListAgents NotebookEdit PushNotification \
    RemoteTrigger ReportFindings SendMessage TaskOutput TaskStop ToolSearch";

/// One turn of the conversation, sent to the actor that owns the claude
/// subprocess. `chunk_tx` carries incremental text as it streams in;
/// `done_tx` resolves once with the final, complete reply text (or an error).
struct Turn {
    system_prompt: String,
    user_text: String,
    chunk_tx: mpsc::UnboundedSender<String>,
    done_tx: oneshot::Sender<Result<String, String>>,
}

/// Owns the resident claude subprocess. Lives entirely inside `actor_loop`,
/// so there's no locking -- turns are simply processed one at a time as they
/// arrive on the channel, which also happens to match `--parallel 1`
/// semantics the original llama-server setup assumed.
struct ClaudeActor {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    lines: Option<Lines<BufReader<ChildStdout>>>,
    spawned_system_prompt: Option<String>,
}

impl ClaudeActor {
    fn new() -> Self {
        Self {
            child: None,
            stdin: None,
            lines: None,
            spawned_system_prompt: None,
        }
    }

    async fn ensure_spawned(&mut self, system_prompt: &str) -> std::io::Result<()> {
        if self.child.is_some() {
            return Ok(());
        }
        let mut cmd = Command::new("claude");
        cmd.args([
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--include-partial-messages",
            "--verbose",
            "--dangerously-skip-permissions",
            "--no-session-persistence",
            "--model",
            CLAUDE_MODEL,
            "--disallowedTools",
            DISALLOWED_TOOLS,
            "--system-prompt",
            system_prompt,
        ]);
        cmd.env("HTTP_PROXY", PROXY_URL);
        cmd.env("HTTPS_PROXY", PROXY_URL);
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::null());

        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().expect("child stdin was piped");
        let stdout = child.stdout.take().expect("child stdout was piped");

        self.stdin = Some(stdin);
        self.lines = Some(BufReader::new(stdout).lines());
        self.child = Some(child);
        self.spawned_system_prompt = Some(system_prompt.to_string());
        Ok(())
    }

    /// Drops the dead/broken child so the next turn respawns a fresh one.
    fn reset(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
        }
        self.stdin = None;
        self.lines = None;
        self.spawned_system_prompt = None;
    }

    async fn run_turn(&mut self, turn: Turn) {
        if let Some(spawned) = &self.spawned_system_prompt {
            if spawned != &turn.system_prompt {
                eprintln!(
                    "[claude-llama-proxy] warning: system prompt changed after the \
                     Claude session already started; the running session keeps using \
                     the prompt from its first turn."
                );
            }
        }

        if let Err(e) = self.ensure_spawned(&turn.system_prompt).await {
            let _ = turn.done_tx.send(Err(format!("failed to start claude: {e}")));
            return;
        }

        let input_line = json!({
            "type": "user",
            "message": { "role": "user", "content": turn.user_text },
        })
        .to_string();

        if let Some(stdin) = self.stdin.as_mut() {
            let write_result = async {
                stdin.write_all(input_line.as_bytes()).await?;
                stdin.write_all(b"\n").await?;
                stdin.flush().await
            }
            .await;
            if let Err(e) = write_result {
                let _ = turn
                    .done_tx
                    .send(Err(format!("failed to write to claude stdin: {e}")));
                self.reset();
                return;
            }
        }

        let lines = match self.lines.as_mut() {
            Some(l) => l,
            None => {
                let _ = turn.done_tx.send(Err("claude process not available".into()));
                return;
            }
        };

        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let event: Value = match serde_json::from_str(&line) {
                        Ok(v) => v,
                        Err(_) => continue, // ignore any non-JSON noise on stdout
                    };
                    match event.get("type").and_then(Value::as_str) {
                        Some("stream_event") => {
                            let is_text_delta = event.pointer("/event/type").and_then(Value::as_str)
                                == Some("content_block_delta");
                            if is_text_delta {
                                if let Some(text) =
                                    event.pointer("/event/delta/text").and_then(Value::as_str)
                                {
                                    let _ = turn.chunk_tx.send(text.to_string());
                                }
                            }
                        }
                        Some("result") => {
                            let result_text = event
                                .get("result")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            let _ = turn.done_tx.send(Ok(result_text));
                            return;
                        }
                        _ => {}
                    }
                }
                Ok(None) => {
                    self.reset();
                    let _ = turn
                        .done_tx
                        .send(Err("claude process exited unexpectedly".into()));
                    return;
                }
                Err(e) => {
                    self.reset();
                    let _ = turn
                        .done_tx
                        .send(Err(format!("error reading claude output: {e}")));
                    return;
                }
            }
        }
    }
}

async fn actor_loop(mut rx: mpsc::UnboundedReceiver<Turn>) {
    let mut actor = ClaudeActor::new();
    while let Some(turn) = rx.recv().await {
        actor.run_turn(turn).await;
    }
}

#[derive(Clone)]
struct AppState {
    tx: mpsc::UnboundedSender<Turn>,
    log_path: PathBuf,
}

#[derive(Deserialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct ChatRequest {
    messages: Vec<ChatMessage>,
    #[serde(default)]
    stream: bool,
}

fn log_line(path: &std::path::Path, role: &str, text: &str) {
    let ts = Local::now().format("%Y-%m-%d %H:%M:%S");
    let line = format!("[{ts}] {role}: {text}\n");
    match std::fs::OpenOptions::new().create(true).append(true).open(path) {
        Ok(mut f) => {
            let _ = f.write_all(line.as_bytes());
        }
        Err(e) => eprintln!("[claude-llama-proxy] could not write log: {e}"),
    }
}

async fn health() -> impl IntoResponse {
    Json(json!({"status": "ok"}))
}

async fn props() -> impl IntoResponse {
    // Real value doesn't matter much -- llm_engine.py only reads this for a
    // log line and tolerates missing fields.
    Json(json!({"default_generation_settings": {"n_ctx": 1_000_000}}))
}

async fn chat_completions(State(state): State<AppState>, Json(req): Json<ChatRequest>) -> Response {
    let system_prompt = req
        .messages
        .iter()
        .find(|m| m.role == "system")
        .map(|m| m.content.clone())
        .unwrap_or_else(|| DEFAULT_SYSTEM_PROMPT.to_string());

    let user_text = match req.messages.iter().rev().find(|m| m.role == "user") {
        Some(m) => m.content.clone(),
        None => return (StatusCode::BAD_REQUEST, "no user message in request").into_response(),
    };

    log_line(&state.log_path, "user", &user_text);

    let (chunk_tx, chunk_rx) = mpsc::unbounded_channel::<String>();
    let (done_tx, done_rx) = oneshot::channel::<Result<String, String>>();

    if state
        .tx
        .send(Turn { system_prompt, user_text, chunk_tx, done_tx })
        .is_err()
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, "proxy actor not running").into_response();
    }

    if req.stream {
        let deltas = UnboundedReceiverStream::new(chunk_rx).map(|text| {
            let payload = json!({"choices": [{"delta": {"content": text}}]});
            Ok::<_, Infallible>(Event::default().data(payload.to_string()))
        });

        let log_path = state.log_path.clone();
        let done = stream::once(async move {
            match done_rx.await {
                Ok(Ok(full_text)) => log_line(&log_path, "assistant", &full_text),
                Ok(Err(e)) => eprintln!("[claude-llama-proxy] turn error: {e}"),
                Err(_) => eprintln!("[claude-llama-proxy] actor dropped mid-turn"),
            }
            Ok::<_, Infallible>(Event::default().data("[DONE]"))
        });

        Sse::new(deltas.chain(done)).into_response()
    } else {
        drop(chunk_rx); // non-streaming caller only wants the final text
        match done_rx.await {
            Ok(Ok(full_text)) => {
                log_line(&state.log_path, "assistant", &full_text);
                Json(json!({
                    "choices": [{"message": {"role": "assistant", "content": full_text}}]
                }))
                .into_response()
            }
            Ok(Err(e)) => (StatusCode::BAD_GATEWAY, e).into_response(),
            Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "actor dropped").into_response(),
        }
    }
}

#[tokio::main]
async fn main() {
    let (tx, rx) = mpsc::unbounded_channel::<Turn>();
    tokio::spawn(actor_loop(rx));

    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let log_dir = PathBuf::from(home).join("claude-llama-proxy").join("logs");
    std::fs::create_dir_all(&log_dir).expect("failed to create log directory");
    let log_path = log_dir.join("conversation.log");

    let state = AppState { tx, log_path };

    let app = Router::new()
        .route("/health", get(health))
        .route("/props", get(props))
        .route("/v1/chat/completions", post(chat_completions))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(LISTEN_ADDR)
        .await
        .unwrap_or_else(|e| panic!("failed to bind {LISTEN_ADDR}: {e}"));
    println!("claude-llama-proxy listening on http://{LISTEN_ADDR}");
    axum::serve(listener, app).await.unwrap();
}
