// Chat through Claude Code — the island's chat talks to the user's own `claude`
// CLI instead of the API, so it runs on their Claude plan and sees the MCP
// connectors they already set up, with no key and nothing to configure here.
//
// One `claude` process lives for the whole conversation and speaks stream-json
// on both ends: a user message goes in on stdin, deltas, tool calls and the
// final result come out on stdout. Starting it costs a couple of seconds, so it
// is started once and kept, not once per message.
//
// What a tool may do without asking is decided here, not by the model: reading
// is allowed, anything that sends, writes or changes something waits for a
// click in the island.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::oneshot;

use crate::claude::{ChatContext, ChatReply};
use crate::{files, island, log, office, platform, settings};

/// The built-in tools the chat gets. Nothing here runs a command or edits a
/// file; `--restricted` keeps the file tools inside the working directories.
const TOOLS: &str = "Read,Glob,Grep,WebSearch,WebFetch,ToolSearch";

/// A tool call can take a while and prints nothing meanwhile; past this the
/// process is considered stuck and the conversation starts over.
const IDLE_TIMEOUT: Duration = Duration::from_secs(180);
/// How long an approval card may wait for a click before it counts as a no.
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(120);
/// The approval card is small: past this the detail is cut.
const MAX_DETAIL: usize = 600;

const SYSTEM_PROMPT: &str = "You are Mochi, a personal assistant living at the top of the user's screen, \
answering in a very small chat window. Respond in the user's language. Be brief and direct: a few lines unless \
the task needs more. No markdown formatting (no **, no ##, no bullet dashes): plain text with line breaks. \
The user's connected services are available to you as tools; use them when the request calls for it. \
Anything that sends, writes or changes something is shown to the user for approval first, so state exactly \
what you are about to do and do not ask for confirmation yourself.";

/// What the island hears while a turn is running.
#[derive(Serialize, Clone)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum Progress {
    Delta { text: String },
    Tool { label: String },
    #[serde(rename_all = "camelCase")]
    Approval { request_id: String, label: String, detail: String },
}

struct Session {
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
}

#[derive(Default)]
pub struct Agent {
    session: tokio::sync::Mutex<Option<Session>>,
    /// Kept apart from the session so `reset` can stop a turn that is running.
    child: Mutex<Option<Child>>,
    approvals: Mutex<HashMap<String, oneshot::Sender<bool>>>,
}

impl Agent {
    /// Ends the conversation: the next message starts a fresh `claude`.
    pub fn reset(&self) {
        for (_, tx) in self.approvals.lock().unwrap().drain() {
            let _ = tx.send(false);
        }
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let _ = child.start_kill();
        }
        // A turn in flight holds the session; it sees the pipe close and drops
        // the session itself.
        if let Ok(mut session) = self.session.try_lock() {
            *session = None;
        }
    }

    /// Called by the Allow / Deny buttons of the chat's approval card.
    pub fn decide(&self, request_id: &str, allow: bool) {
        log::line(format!("chat decision id={request_id} {}", if allow { "allow" } else { "deny" }));
        if let Some(tx) = self.approvals.lock().unwrap().remove(request_id) {
            let _ = tx.send(allow);
        }
    }
}

/// Where `claude` is, if Claude Code is installed. The native installer puts it
/// in ~/.local/bin, which is not always on the PATH a GUI app is started with.
pub fn executable() -> Option<PathBuf> {
    if let Some(found) = platform::find_on_path("claude") {
        return Some(found);
    }
    let bin = platform::home_dir().join(".local").join("bin");
    ["claude.exe", "claude"].iter().map(|name| bin.join(name)).find(|p| p.is_file())
}

/// One chat turn through Claude Code. Returns the assistant's text, or a
/// message the island shows in the note view.
pub async fn send(
    app: &AppHandle,
    agent: &Agent,
    exe: &Path,
    query: String,
    context: Option<ChatContext>,
) -> Result<ChatReply, String> {
    let mut guard = agent.session.lock().await;
    if guard.is_none() {
        let (child, session) = spawn(exe)?;
        *agent.child.lock().unwrap() = Some(child);
        *guard = Some(session);
    }
    let session = guard.as_mut().expect("session was just set");

    let outcome = turn(app, agent, session, compose(query, context)).await;
    if outcome.is_err() {
        // The stream is in an unknown state: start over on the next message.
        *guard = None;
        if let Some(mut child) = agent.child.lock().unwrap().take() {
            let _ = child.start_kill();
        }
    }
    outcome
}

fn spawn(exe: &Path) -> Result<(Child, Session), String> {
    // Its own empty folder, so no project's CLAUDE.md or settings leak into the
    // chat; the inbox is added so a dropped file can be read.
    let cwd = settings::local_dir().join("chat");
    let inbox = files::inbox_dir();
    platform::ensure_private_dir(&settings::local_dir()).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&cwd).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&inbox).map_err(|e| e.to_string())?;

    let mut cmd = std::process::Command::new(exe);
    cmd.args([
        "-p",
        "--input-format", "stream-json",
        "--output-format", "stream-json",
        "--verbose",
        "--include-partial-messages",
        // Permission prompts come to us on stdout instead of being denied.
        "--permission-prompt-tool", "stdio",
        // No hooks, plugins or user settings: Coucou's own hooks must not fire
        // for its own chat, and the chat must not inherit a coding setup.
        "--restricted",
        "--no-session-persistence",
        "--tools", TOOLS,
        "--append-system-prompt", SYSTEM_PROMPT,
    ]);
    cmd.arg("--add-dir").arg(&inbox).current_dir(&cwd);
    platform::no_console(&mut cmd);

    let mut cmd = Command::from(cmd);
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| format!("Could not start Claude Code: {e}"))?;
    let stdin = child.stdin.take().ok_or("Claude Code has no stdin.")?;
    let stdout = child.stdout.take().ok_or("Claude Code has no stdout.")?;
    log::line("chat: Claude Code started");
    Ok((child, Session { stdin, lines: BufReader::new(stdout).lines() }))
}

/// The dropped file is named by its path: Claude Code reads it itself, so its
/// bytes never pass through here. An Office document is the exception — Claude
/// Code cannot open one, so it is pointed at a text copy instead.
fn compose(query: String, context: Option<ChatContext>) -> String {
    match context {
        Some(ChatContext::File { name, path }) => {
            let note = match office::text_copy(Path::new(&path)) {
                None => format!("The user dropped this file: {path} (named {name}). Read it if the request needs it."),
                Some(Ok(copy)) => format!(
                    "The user dropped the document {name}. Its text was extracted to {}; read that file, not the \
                     original. It is text only: layout, images and charts are not in it.",
                    copy.display()
                ),
                Some(Err(why)) => format!(
                    "The user dropped the document {name}, but its text could not be read: {why}. Tell them so."
                ),
            };
            format!("{note}\n\n{query}")
        }
        Some(ChatContext::Window { app_name, title, url }) => {
            let url = url.map(|u| format!(", URL: {u}")).unwrap_or_default();
            format!("Context — App: {app_name}, Window: {title}{url}\n\n{query}")
        }
        None => query,
    }
}

async fn write_line(session: &mut Session, message: &Value) -> Result<(), String> {
    let mut line = message.to_string();
    line.push('\n');
    let gone = |_| "Claude Code closed the conversation.".to_string();
    session.stdin.write_all(line.as_bytes()).await.map_err(gone)?;
    session.stdin.flush().await.map_err(gone)
}

async fn turn(
    app: &AppHandle,
    agent: &Agent,
    session: &mut Session,
    text: String,
) -> Result<ChatReply, String> {
    write_line(session, &json!({ "type": "user", "message": { "role": "user", "content": text } })).await?;

    loop {
        let line = match tokio::time::timeout(IDLE_TIMEOUT, session.lines.next_line()).await {
            Err(_) => return Err("Claude Code stopped answering.".into()),
            Ok(Ok(Some(line))) => line,
            Ok(_) => return Err("Claude Code closed the conversation.".into()),
        };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };

        match msg.get("type").and_then(Value::as_str) {
            Some("stream_event") => {
                let delta = &msg["event"]["delta"];
                if delta["type"] == "text_delta" {
                    if let Some(text) = delta["text"].as_str() {
                        emit(app, Progress::Delta { text: text.to_string() });
                    }
                }
            }
            Some("assistant") => {
                for block in msg["message"]["content"].as_array().into_iter().flatten() {
                    if block["type"] != "tool_use" {
                        continue;
                    }
                    let name = block["name"].as_str().unwrap_or_default();
                    // Looking up a tool is plumbing, not something worth showing.
                    if name != "ToolSearch" {
                        emit(app, Progress::Tool { label: tool_label(name, None, None) });
                    }
                }
            }
            Some("control_request") => {
                let response = control_response(app, agent, &msg).await;
                write_line(session, &response).await?;
            }
            Some("result") => {
                let text = msg["result"].as_str().unwrap_or_default().trim().to_string();
                let failed = msg["is_error"].as_bool().unwrap_or(false) || msg["subtype"] != "success";
                if failed {
                    return Err(if text.is_empty() { "Claude Code could not answer.".into() } else { text });
                }
                if text.is_empty() {
                    return Err("No response text.".into());
                }
                return Ok(ChatReply { text });
            }
            _ => {}
        }
    }
}

fn emit(app: &AppHandle, progress: Progress) {
    let _ = app.emit_to(island::WINDOW_LABEL, "chat-progress", progress);
}

/// Answers a request Claude Code makes of its host. The only one that matters
/// is "may I use this tool"; anything else is refused rather than left hanging.
async fn control_response(app: &AppHandle, agent: &Agent, msg: &Value) -> Value {
    let id = msg["request_id"].as_str().unwrap_or_default().to_string();
    let request = &msg["request"];
    if request["subtype"] != "can_use_tool" {
        return json!({
            "type": "control_response",
            "response": { "subtype": "error", "request_id": id, "error": "Not supported by Coucou." },
        });
    }

    let tool = request["tool_name"].as_str().unwrap_or_default();
    let input = request["input"].clone();
    let allow = if is_read_only(tool) {
        true
    } else {
        let label = tool_label(
            tool,
            request["display_name"].as_str(),
            request["mcp_server"]["name"].as_str(),
        );
        ask(app, agent, &id, tool, label, input_detail(&input)).await
    };

    let verdict = if allow {
        json!({ "behavior": "allow", "updatedInput": input })
    } else {
        json!({ "behavior": "deny", "message": "The user declined this action." })
    };
    json!({
        "type": "control_response",
        "response": { "subtype": "success", "request_id": id, "response": verdict },
    })
}

/// Puts the approval card up and waits for the click. No click is a no.
async fn ask(app: &AppHandle, agent: &Agent, id: &str, tool: &str, label: String, detail: String) -> bool {
    let (tx, rx) = oneshot::channel();
    agent.approvals.lock().unwrap().insert(id.to_string(), tx);
    // The tool's name only: what it is about to send stays out of the log.
    log::line(format!("chat approval id={id} tool={tool}"));
    emit(app, Progress::Approval { request_id: id.to_string(), label, detail });

    match tokio::time::timeout(APPROVAL_TIMEOUT, rx).await {
        Ok(Ok(allow)) => allow,
        _ => {
            agent.approvals.lock().unwrap().remove(id);
            log::line(format!("chat approval id={id} timed out — denied"));
            false
        }
    }
}

// ── What may run without asking ───────────────────────────────────────────────

const READ_WORDS: &[&str] = &[
    "get", "list", "search", "query", "read", "find", "fetch", "describe", "show", "lookup",
    "resolve", "check", "count", "view", "explore", "validate", "preview",
];

const WRITE_WORDS: &[&str] = &[
    "create", "send", "delete", "update", "edit", "add", "remove", "set", "post", "reply",
    "forward", "move", "copy", "rename", "upload", "publish", "execute", "run", "trash",
    "untrash", "modify", "respond", "close", "revoke", "archive", "unarchive", "activate",
    "deactivate", "boost", "restore", "generate", "import", "export", "write", "insert",
    "drop", "cancel", "approve", "start", "stop", "trigger", "share", "save", "log",
    "connect", "disconnect", "unpublish", "pin", "sign", "pick", "authenticate", "complete",
];

/// Whether a tool may run without a click. The built-in tools the chat has only
/// read. A connector's tool is judged by the words in its name, and a name that
/// says nothing clear is treated as writing: a needless card costs a click, a
/// message sent unasked cannot be taken back.
fn is_read_only(tool: &str) -> bool {
    let Some(rest) = tool.strip_prefix("mcp__") else {
        return TOOLS.split(',').any(|t| t == tool);
    };
    // mcp__<server>__<tool>: only the tool's own name counts.
    let name = rest.split_once("__").map(|(_, name)| name).unwrap_or(rest).to_lowercase();
    let words: Vec<&str> = name.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).collect();
    words.iter().any(|w| READ_WORDS.contains(w)) && !words.iter().any(|w| WRITE_WORDS.contains(w))
}

/// "Microsoft 365 · Send Chat Message" from what Claude Code tells us, or from
/// the raw `mcp__claude_ai_Microsoft_365__teams_send_chat_message` otherwise.
fn tool_label(tool: &str, display_name: Option<&str>, server: Option<&str>) -> String {
    let Some(rest) = tool.strip_prefix("mcp__") else {
        return tool.to_string();
    };
    let (raw_server, raw_name) = rest.split_once("__").unwrap_or((rest, ""));
    let server = server
        .map(|s| s.strip_prefix("claude.ai ").unwrap_or(s).to_string())
        .unwrap_or_else(|| raw_server.strip_prefix("claude_ai_").unwrap_or(raw_server).replace('_', " "));
    let name = display_name.map(str::to_string).unwrap_or_else(|| raw_name.replace(['_', '-'], " "));
    if name.is_empty() { server } else { format!("{server} · {name}") }
}

/// The tool's input as "field: value" lines — what is about to be sent, in a
/// form a person can read before clicking Allow.
fn input_detail(input: &Value) -> String {
    let text = match input.as_object() {
        Some(fields) => fields
            .iter()
            .filter(|(_, v)| !v.is_null())
            .map(|(k, v)| match v.as_str() {
                Some(s) => format!("{k}: {s}"),
                None => format!("{k}: {v}"),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        None => input.to_string(),
    };
    if text.chars().count() <= MAX_DETAIL {
        return text;
    }
    let cut: String = text.chars().take(MAX_DETAIL).collect();
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reading_tools_run_without_a_card() {
        for tool in [
            "Read",
            "WebSearch",
            "ToolSearch",
            "mcp__claude_ai_Microsoft_365__get_me",
            "mcp__claude_ai_Microsoft_365__chat_message_search",
            "mcp__claude_ai_Microsoft_365__outlook_calendar_search",
            "mcp__claude_ai_Microsoft_365__teams_list_chats",
            "mcp__claude_ai_Microsoft_365__find_meeting_availability",
            "mcp__claude_ai_Microsoft_365__read_resource",
            "mcp__claude_ai_crm__query_bitacora",
            "mcp__claude_ai_memoria__search_memories",
            "mcp__claude_ai_Context7__resolve-library-id",
        ] {
            assert!(is_read_only(tool), "{tool} only reads and should not ask");
        }
    }

    #[test]
    fn anything_that_sends_or_changes_waits_for_a_click() {
        for tool in [
            "Bash",
            "Write",
            "mcp__claude_ai_Microsoft_365__teams_send_chat_message",
            "mcp__claude_ai_Microsoft_365__teams_reply_channel_message",
            "mcp__claude_ai_Microsoft_365__outlook_create_event",
            "mcp__claude_ai_Microsoft_365__outlook_send_mail",
            "mcp__claude_ai_Microsoft_365__outlook_respond_to_event",
            "mcp__claude_ai_Microsoft_365__outlook_set_vacation",
            "mcp__claude_ai_crm__create_bitacora",
            "mcp__claude_ai_memoria__add_memory",
            "mcp__claude_ai_n8n__execute_workflow",
            // A read word next to a write word is still a write.
            "mcp__claude_ai_Meta__ads_update_custom_audience_users",
            "mcp__claude_ai_n8n__get_and_delete_rows",
            // Says nothing clear: ask.
            "mcp__someone__do_the_thing",
            "mcp__weird",
        ] {
            assert!(!is_read_only(tool), "{tool} must ask first");
        }
    }

    #[test]
    fn a_server_called_get_does_not_make_its_tools_readers() {
        assert!(!is_read_only("mcp__get_search__send_message"));
    }

    #[test]
    fn labels_read_like_names() {
        assert_eq!(
            tool_label(
                "mcp__claude_ai_Microsoft_365__teams_send_chat_message",
                Some("Send Chat Message"),
                Some("claude.ai Microsoft 365"),
            ),
            "Microsoft 365 · Send Chat Message"
        );
        assert_eq!(
            tool_label("mcp__claude_ai_Microsoft_365__get_me", None, None),
            "Microsoft 365 · get me"
        );
        assert_eq!(tool_label("WebSearch", None, None), "WebSearch");
    }

    #[test]
    fn the_detail_shows_what_is_about_to_be_sent() {
        let detail = input_detail(&json!({ "chatId": "19:abc", "content": "Voy en camino", "urgent": false, "skip": null }));
        assert_eq!(detail, "chatId: 19:abc\ncontent: Voy en camino\nurgent: false");
    }

    #[test]
    fn a_long_detail_is_cut_on_a_char_boundary() {
        let detail = input_detail(&json!({ "content": "é".repeat(2000) }));
        assert_eq!(detail.chars().count(), MAX_DETAIL + 1);
        assert!(detail.ends_with('…'));
    }

    #[test]
    fn a_dropped_file_is_named_by_its_path() {
        let text = compose(
            "resume esto".into(),
            Some(ChatContext::File { name: "a.pdf".into(), path: "C:\\inbox\\a.pdf".into() }),
        );
        assert!(text.contains("C:\\inbox\\a.pdf") && text.ends_with("resume esto"));
        assert_eq!(compose("hola".into(), None), "hola");
    }
}
