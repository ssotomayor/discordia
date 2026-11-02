use std::future::Future;
use std::time::Duration;

use serde_json::{Value, json};

use crate::config::{ModelConfig, Provider};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub role: Role,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub system: String,
    /// Alternating, starting with `User`: `context::build` guarantees it.
    pub turns: Vec<Turn>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub text: String,
    pub tokens: u64,
}

#[derive(Debug)]
pub enum HarnessError {
    Network(String),
    Status(u16, String),
    Decode(String),
    /// The model declined; it is answered, not retried.
    Refused {
        tokens: u64,
    },
    Empty {
        tokens: u64,
    },
}

impl HarnessError {
    pub fn tokens(&self) -> u64 {
        match self {
            Self::Refused { tokens } | Self::Empty { tokens } => *tokens,
            _ => 0,
        }
    }
}

impl std::fmt::Display for HarnessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network(e) => write!(f, "network: {e}"),
            Self::Status(code, body) => write!(f, "provider answered {code}: {body}"),
            Self::Decode(e) => write!(f, "unreadable response: {e}"),
            Self::Refused { .. } => f.write_str("the model declined"),
            Self::Empty { .. } => f.write_str("the model returned no text"),
        }
    }
}

/// Whatever turns a prompt into a reply. `Http` is the real one; tests plug in their own.
pub trait Harness: Send + Sync + 'static {
    fn reply(&self, prompt: Prompt) -> impl Future<Output = Result<Reply, HarnessError>> + Send;
}

#[derive(Clone)]
pub struct Http {
    client: reqwest::Client,
    config: ModelConfig,
}

/// Provider error bodies are echoed into logs; long ones are cut.
const ERROR_BODY_CHARS: usize = 300;

impl Http {
    pub fn new(config: ModelConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .unwrap_or_default();
        Self { client, config }
    }

    fn url(&self, path: &str) -> String {
        let base = self.config.base_url.trim_end_matches('/');
        match self.config.provider {
            Provider::Anthropic => format!("{base}/v1/{path}"),
            Provider::OpenAiCompatible => format!("{base}/{path}"),
        }
    }

    fn authed(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let key = self.config.api_key.expose();
        match self.config.provider {
            Provider::Anthropic => request
                .header("x-api-key", key)
                .header("anthropic-version", ANTHROPIC_VERSION),
            // A local server (Ollama) takes no key; an empty bearer is refused by some.
            Provider::OpenAiCompatible if key.is_empty() => request,
            Provider::OpenAiCompatible => request.bearer_auth(key),
        }
    }

    async fn send(&self, request: reqwest::RequestBuilder) -> Result<Value, HarnessError> {
        let response = self
            .authed(request)
            .send()
            .await
            .map_err(|e| HarnessError::Network(e.without_url().to_string()))?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(HarnessError::Status(
                status.as_u16(),
                text.chars().take(ERROR_BODY_CHARS).collect(),
            ));
        }
        response
            .json::<Value>()
            .await
            .map_err(|e| HarnessError::Decode(e.without_url().to_string()))
    }

    /// The models this key may use, for a picker. Both provider kinds answer
    /// `{"data": [{"id": …}]}`.
    pub async fn models(&self) -> Result<Vec<String>, HarnessError> {
        let request = self
            .client
            .get(self.url("models"))
            .query(&[("limit", "100")]);
        Ok(parse_models(self.send(request).await?))
    }
}

impl Harness for Http {
    async fn reply(&self, prompt: Prompt) -> Result<Reply, HarnessError> {
        match self.config.provider {
            Provider::Anthropic => {
                let request = self
                    .client
                    .post(self.url("messages"))
                    .json(&anthropic_body(&self.config, &prompt));
                parse_anthropic(self.send(request).await?)
            }
            Provider::OpenAiCompatible => {
                let request = self
                    .client
                    .post(self.url("chat/completions"))
                    .json(&openai_body(&self.config, &prompt));
                parse_openai(self.send(request).await?)
            }
        }
    }
}

fn parse_models(v: Value) -> Vec<String> {
    let mut ids: Vec<String> = v["data"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|m| m["id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids.dedup();
    ids
}

const ANTHROPIC_VERSION: &str = "2026-10-21";

fn role(r: Role) -> &'static str {
    match r {
        Role::User => "user",
        Role::Assistant => "assistant",
    }
}

fn anthropic_body(config: &ModelConfig, prompt: &Prompt) -> Value {
    let messages: Vec<Value> = prompt
        .turns
        .iter()
        .map(|t| json!({ "role": role(t.role), "content": t.text }))
        .collect();
    json!({
        "model": config.model,
        "max_tokens": config.max_output_tokens,
        "system": prompt.system,
        "messages": messages,
    })
}

fn parse_anthropic(v: Value) -> Result<Reply, HarnessError> {
    let usage = &v["usage"];
    let tokens =
        usage["input_tokens"].as_u64().unwrap_or(0) + usage["output_tokens"].as_u64().unwrap_or(0);
    if v["stop_reason"] == "refusal" {
        return Err(HarnessError::Refused { tokens });
    }
    let blocks = v["content"]
        .as_array()
        .ok_or_else(|| HarnessError::Decode("no content array".into()))?;
    let text = blocks
        .iter()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect::<Vec<_>>()
        .join("")
        .trim()
        .to_string();
    if text.is_empty() {
        return Err(HarnessError::Empty { tokens });
    }
    Ok(Reply { text, tokens })
}

fn openai_body(config: &ModelConfig, prompt: &Prompt) -> Value {
    let mut messages = vec![json!({ "role": "system", "content": prompt.system })];
    messages.extend(
        prompt
            .turns
            .iter()
            .map(|t| json!({ "role": role(t.role), "content": t.text })),
    );
    json!({
        "model": config.model,
        "max_tokens": config.max_output_tokens,
        "messages": messages,
    })
}

fn parse_openai(v: Value) -> Result<Reply, HarnessError> {
    let tokens = v["usage"]["total_tokens"].as_u64().unwrap_or(0);
    let choice = &v["choices"][0];
    if choice.is_null() {
        return Err(HarnessError::Decode("no choices".into()));
    }
    if choice["finish_reason"] == "content_filter" {
        return Err(HarnessError::Refused { tokens });
    }
    let text = choice["message"]["content"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_string();
    if text.is_empty() {
        return Err(HarnessError::Empty { tokens });
    }
    Ok(Reply { text, tokens })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ApiKey;

    fn prompt() -> Prompt {
        Prompt {
            system: "sys".into(),
            turns: vec![
                Turn {
                    role: Role::User,
                    text: "Ana: hi @Helper".into(),
                },
                Turn {
                    role: Role::Assistant,
                    text: "hello".into(),
                },
                Turn {
                    role: Role::User,
                    text: "Ana: thanks".into(),
                },
            ],
        }
    }

    #[test]
    fn anthropic_requests_carry_the_system_apart_and_no_sampling_fields() {
        let config = ModelConfig::anthropic("claude-opus-5-5", ApiKey::new("k"));
        let body = anthropic_body(&config, &prompt());
        assert_eq!(body["model"], "claude-opus-5-5");
        assert_eq!(body["system"], "sys");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][1]["role"], "assistant");
        assert_eq!(body["messages"].as_array().unwrap().len(), 3);
        assert!(body.get("temperature").is_none());
        assert!(!body.to_string().contains("\"k\""), "no key in the body");
    }

    #[test]
    fn anthropic_replies_join_text_blocks_and_count_both_directions() {
        let reply = parse_anthropic(json!({
            "content": [
                { "type": "thinking", "thinking": "" },
                { "type": "text", "text": "Hel" },
                { "type": "text", "text": "lo " }
            ],
            "stop_reason": "end_turn",
            "usage": { "input_tokens": 10, "output_tokens": 5 }
        }))
        .unwrap();
        assert_eq!(
            reply,
            Reply {
                text: "Hello".into(),
                tokens: 15
            }
        );
    }

    #[test]
    fn an_anthropic_refusal_is_its_own_outcome() {
        let err = parse_anthropic(json!({
            "content": [], "stop_reason": "refusal",
            "usage": { "input_tokens": 7, "output_tokens": 0 }
        }))
        .unwrap_err();
        assert!(matches!(err, HarnessError::Refused { tokens: 7 }));
        assert!(matches!(
            parse_anthropic(json!({ "content": [], "usage": {} })).unwrap_err(),
            HarnessError::Empty { .. }
        ));
    }

    #[test]
    fn openai_requests_put_the_system_first() {
        let config = ModelConfig::openai_compatible(
            "Ollama",
            "http://127.0.0.1:11434/v1",
            "llama3",
            ApiKey::new(""),
        );
        let body = openai_body(&config, &prompt());
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["content"], "Ana: hi @Helper");
        assert_eq!(body["messages"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn model_lists_read_ids_from_either_provider() {
        let ids = parse_models(json!({
            "data": [{ "id": "b-model" }, { "id": "a-model" }, { "id": "a-model" }, { "name": "x" }]
        }));
        assert_eq!(ids, vec!["a-model", "b-model"]);
        assert!(parse_models(json!({ "error": "nope" })).is_empty());
    }

    #[test]
    fn each_provider_has_its_own_paths() {
        let a = Http::new(ModelConfig::anthropic("m", ApiKey::new("k")));
        assert_eq!(a.url("messages"), "https://api.anthropic.com/v1/messages");
        let o = Http::new(ModelConfig::openai_compatible(
            "Ollama",
            "http://127.0.0.1:11434/v1/",
            "m",
            ApiKey::new(""),
        ));
        assert_eq!(
            o.url("chat/completions"),
            "http://127.0.0.1:11434/v1/chat/completions"
        );
        assert_eq!(o.url("models"), "http://127.0.0.1:11434/v1/models");
    }

    #[test]
    fn openai_replies_and_filters() {
        let ok = parse_openai(json!({
            "choices": [{ "message": { "content": " hey " }, "finish_reason": "stop" }],
            "usage": { "total_tokens": 9 }
        }))
        .unwrap();
        assert_eq!(
            ok,
            Reply {
                text: "hey".into(),
                tokens: 9
            }
        );
        let filtered = parse_openai(json!({
            "choices": [{ "message": { "content": null }, "finish_reason": "content_filter" }]
        }))
        .unwrap_err();
        assert!(matches!(filtered, HarnessError::Refused { .. }));
        assert!(matches!(
            parse_openai(json!({})).unwrap_err(),
            HarnessError::Decode(_)
        ));
    }
}
