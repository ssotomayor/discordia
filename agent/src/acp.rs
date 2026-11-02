//! A coding agent (Claude Code, Codex, Goose) behind an agent, over the Agent
//! Client Protocol on stdio (#457). Started per reply, so nothing one person
//! asked carries into another's turn.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{ChildStdin, ChildStdout, Command};

use crate::harness::{Harness, HarnessError, Prompt, Reply, Role};

const PROTOCOL_VERSION: u16 = 1;
/// A coding agent may think for minutes; a turn still has to end.
const TURN_LIMIT: Duration = Duration::from_secs(300);

#[derive(Clone, Debug)]
pub struct Acp {
    program: String,
    args: Vec<String>,
    /// Its own empty folder, so relative paths land nowhere that matters.
    cwd: PathBuf,
}

impl Acp {
    /// Split on whitespace, never through a shell.
    pub fn new(command_line: &str, cwd: PathBuf) -> Result<Self, String> {
        let mut words = command_line.split_whitespace().map(str::to_string);
        let program = words
            .next()
            .ok_or("name the command that starts the agent")?;
        Ok(Self {
            program,
            args: words.collect(),
            cwd,
        })
    }

    pub fn program(&self) -> &str {
        &self.program
    }

    async fn turn(&self, prompt: Prompt) -> Result<Reply, HarnessError> {
        std::fs::create_dir_all(&self.cwd).map_err(|e| HarnessError::Network(e.to_string()))?;
        let cwd = std::fs::canonicalize(&self.cwd).unwrap_or_else(|_| self.cwd.clone());
        let mut child = Command::new(&self.program)
            .args(&self.args)
            .current_dir(&cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| HarnessError::Network(format!("couldn't start {}: {e}", self.program)))?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(HarnessError::Network("the agent has no stdio".into()));
        };
        let mut rpc = Rpc {
            stdin,
            lines: BufReader::new(stdout).lines(),
            text: String::new(),
        };

        rpc.call(
            1,
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "clientCapabilities": {
                    "fs": { "readTextFile": false, "writeTextFile": false },
                    "terminal": false,
                },
                "clientInfo": { "name": "discordia", "version": env!("CARGO_PKG_VERSION") },
            }),
        )
        .await?;
        let session = rpc
            .call(2, "session/new", json!({ "cwd": cwd, "mcpServers": [] }))
            .await?;
        let session_id = session["sessionId"]
            .as_str()
            .ok_or_else(|| HarnessError::Decode("no sessionId".into()))?
            .to_string();
        let done = rpc
            .call(
                3,
                "session/prompt",
                json!({
                    "sessionId": session_id,
                    "prompt": [{ "type": "text", "text": render(&prompt) }],
                }),
            )
            .await?;
        let text = rpc.text.trim().to_string();
        if done["stopReason"] == "refusal" {
            return Err(HarnessError::Refused { tokens: 0 });
        }
        if text.is_empty() {
            return Err(HarnessError::Empty { tokens: 0 });
        }
        Ok(Reply { text, tokens: 0 })
    }
}

impl Harness for Acp {
    async fn reply(&self, prompt: Prompt) -> Result<Reply, HarnessError> {
        tokio::time::timeout(TURN_LIMIT, self.turn(prompt))
            .await
            .map_err(|_| HarnessError::Network("the agent took too long".into()))?
    }
}

/// One conversation as one message: a coding agent takes a prompt, not turns.
fn render(prompt: &Prompt) -> String {
    let mut out = prompt.system.clone();
    out.push_str("\n\nThe conversation so far:\n");
    for t in &prompt.turns {
        match t.role {
            Role::User => out.push_str(&t.text),
            Role::Assistant => {
                out.push_str("You: ");
                out.push_str(&t.text);
            }
        }
        out.push('\n');
    }
    out.push_str("\nWrite your reply to the last message as plain chat text.");
    out
}

struct Rpc {
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    /// What the agent said, gathered from `agent_message_chunk` updates.
    text: String,
}

impl Rpc {
    async fn write(&mut self, message: Value) -> Result<(), HarnessError> {
        let mut line = message.to_string();
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| HarnessError::Network(e.to_string()))
    }

    /// Sends a request and serves the agent's own requests until it answers.
    async fn call(&mut self, id: u64, method: &str, params: Value) -> Result<Value, HarnessError> {
        self.write(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await?;
        loop {
            let line = self
                .lines
                .next_line()
                .await
                .map_err(|e| HarnessError::Network(e.to_string()))?
                .ok_or_else(|| HarnessError::Network("the agent exited".into()))?;
            let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if let Some(method) = msg["method"].as_str() {
                self.serve(method, &msg).await?;
                continue;
            }
            if msg["id"].as_u64() != Some(id) {
                continue;
            }
            if let Some(error) = msg.get("error") {
                return Err(HarnessError::Status(
                    error["code"].as_i64().unwrap_or(0) as u16,
                    error["message"]
                        .as_str()
                        .unwrap_or("")
                        .chars()
                        .take(300)
                        .collect(),
                ));
            }
            return Ok(msg["result"].clone());
        }
    }

    async fn serve(&mut self, method: &str, msg: &Value) -> Result<(), HarnessError> {
        match method {
            "session/update" => {
                let update = &msg["params"]["update"];
                if update["sessionUpdate"] == "agent_message_chunk"
                    && update["content"]["type"] == "text"
                    && let Some(chunk) = update["content"]["text"].as_str()
                {
                    self.text.push_str(chunk);
                }
                Ok(())
            }
            // Whoever triggered it is not at this computer to say yes: every tool is refused.
            "session/request_permission" => {
                let refuse = msg["params"]["options"]
                    .as_array()
                    .and_then(|options| {
                        options.iter().find(|o| {
                            matches!(o["kind"].as_str(), Some("reject_once" | "reject_always"))
                        })
                    })
                    .and_then(|o| o["optionId"].as_str());
                let outcome = match refuse {
                    Some(option) => json!({ "outcome": "selected", "optionId": option }),
                    None => json!({ "outcome": "cancelled" }),
                };
                self.write(
                    json!({ "jsonrpc": "2.0", "id": msg["id"], "result": { "outcome": outcome } }),
                )
                .await
            }
            _ if !msg["id"].is_null() => {
                self.write(json!({
                    "jsonrpc": "2.0",
                    "id": msg["id"],
                    "error": { "code": -32601, "message": "not offered by this client" },
                }))
                .await
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::Turn;

    /// A stand-in agent: asks for a permission and a file, then answers.
    const FAKE_AGENT: &str = r#"
import json, sys
def send(m): sys.stdout.write(json.dumps(m) + "\n"); sys.stdout.flush()
def recv(): return json.loads(sys.stdin.readline())
init = recv(); send({"jsonrpc": "2.0", "id": init["id"], "result": {"protocolVersion": 1}})
new = recv(); send({"jsonrpc": "2.0", "id": new["id"], "result": {"sessionId": "s1"}})
prompt = recv()
send({"jsonrpc": "2.0", "id": 90, "method": "session/request_permission", "params": {"sessionId": "s1", "toolCall": {}, "options": [
  {"optionId": "yes", "name": "Allow", "kind": "allow_once"}, {"optionId": "no", "name": "Deny", "kind": "reject_once"}]}})
perm = recv()
send({"jsonrpc": "2.0", "id": 91, "method": "fs/read_text_file", "params": {"sessionId": "s1", "path": "/etc/hosts"}})
fs = recv()
said = "perm=" + perm["result"]["outcome"]["optionId"] + " fs=" + str(fs["error"]["code"])
asked = prompt["params"]["prompt"][0]["text"].splitlines()[-3]
for chunk in [said, " | ", asked]:
    send({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "s1", "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": chunk}}}})
send({"jsonrpc": "2.0", "id": prompt["id"], "result": {"stopReason": "end_turn"}})
"#;

    fn python() -> Option<&'static str> {
        ["python3", "python"].into_iter().find(|p| {
            std::process::Command::new(p)
                .arg("--version")
                .output()
                .is_ok_and(|o| o.status.success())
        })
    }

    #[tokio::test]
    async fn every_tool_is_refused_and_the_answer_is_gathered_from_its_chunks() {
        let Some(python) = python() else {
            eprintln!("no python: skipped");
            return;
        };
        let dir = std::env::temp_dir().join(format!("discordia-acp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("fake_agent.py");
        std::fs::write(&script, FAKE_AGENT).unwrap();
        let acp = Acp::new(&format!("{python} {}", script.display()), dir.join("work")).unwrap();
        let reply = acp
            .reply(Prompt {
                system: "sys".into(),
                turns: vec![Turn {
                    role: Role::User,
                    text: "Ana: what is two plus two?".into(),
                }],
            })
            .await
            .unwrap();
        assert_eq!(reply.text, "perm=no fs=-32601 | Ana: what is two plus two?");
    }

    #[tokio::test]
    async fn a_missing_program_is_an_error_not_a_hang() {
        let acp = Acp::new("definitely-not-a-real-agent-binary", std::env::temp_dir()).unwrap();
        let err = acp
            .reply(Prompt {
                system: String::new(),
                turns: vec![],
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("couldn't start"), "{err}");
        assert!(Acp::new("   ", std::env::temp_dir()).is_err());
    }
}
