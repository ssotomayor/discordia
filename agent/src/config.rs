use std::fmt;

use discordia_protocol::{
    AgentInfo, AppManifest, BotCommand, CommandOption, Intent, MAX_APP_NAME, MAX_COMMAND_TEXT,
    Permission, sanitize_line,
};

/// Configs get logged and printed with `{:?}`; a key must never be.
#[derive(Clone)]
pub struct ApiKey(String);

impl ApiKey {
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(redacted)")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Anthropic,
    /// `POST {base_url}/chat/completions`: OpenAI, OpenRouter, Ollama, vLLM, LM Studio.
    OpenAiCompatible,
}

#[derive(Debug, Clone)]
pub struct ModelConfig {
    pub provider: Provider,
    /// What members are told their messages are sent to.
    pub provider_label: String,
    pub base_url: String,
    pub model: String,
    pub api_key: ApiKey,
    pub max_output_tokens: u32,
}

impl ModelConfig {
    pub fn anthropic(model: impl Into<String>, api_key: ApiKey) -> Self {
        Self {
            provider: Provider::Anthropic,
            provider_label: "Anthropic".into(),
            base_url: "https://api.anthropic.com".into(),
            model: model.into(),
            api_key,
            max_output_tokens: 4096,
        }
    }

    pub fn openai_compatible(
        provider_label: impl Into<String>,
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: ApiKey,
    ) -> Self {
        Self {
            provider: Provider::OpenAiCompatible,
            provider_label: provider_label.into(),
            base_url: base_url.into(),
            model: model.into(),
            api_key,
            max_output_tokens: 4096,
        }
    }
}

/// Whoever triggers an agent spends its owner's money, so every cap is on.
#[derive(Debug, Clone)]
pub struct Limits {
    pub per_member_per_hour: u32,
    /// Input plus output, as the provider reports it, per UTC day.
    pub daily_tokens: u64,
    /// The most any one guild may spend of that each day.
    pub guild_daily_tokens: Option<u64>,
    pub in_flight: usize,
    pub context_messages: u32,
    /// An estimate (a token is about four bytes), not the provider's count.
    pub context_tokens: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            per_member_per_hour: 20,
            daily_tokens: 200_000,
            guild_daily_tokens: None,
            in_flight: 4,
            context_messages: 30,
            context_tokens: 8_000,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub name: String,
    pub prompt: String,
    pub model: ModelConfig,
    pub limits: Limits,
    /// Bump with any change to what the manifest asks for or discloses.
    pub manifest_version: u64,
    /// Answer this key only. A coding agent on its creator's computer is set
    /// to its creator, or any member could drive a shell there (#457).
    pub only_from: Option<String>,
}

pub(crate) const ASK: &str = "ask";

impl AgentConfig {
    pub fn commands(&self) -> Vec<BotCommand> {
        vec![
            BotCommand::new(ASK, "Ask")
                .description("Ask privately; only you see the answer")
                .option(CommandOption::text(
                    "question",
                    "Question",
                    MAX_COMMAND_TEXT,
                )),
        ]
    }

    pub fn manifest(&self) -> AppManifest {
        AppManifest {
            name: sanitize_line(&self.name, MAX_APP_NAME),
            description: format!(
                "AI agent. What it reads is sent to {} ({}).",
                self.model.provider_label, self.model.model
            ),
            icon: "🤖".into(),
            version: self.manifest_version,
            permissions: vec![Permission::SendMessages, Permission::ReadMessageHistory],
            intents: vec![Intent::GuildMessages, Intent::MessageContent],
            commands: self.commands(),
            activity: None,
            agent: Some(AgentInfo {
                provider: self.model.provider_label.clone(),
                model: self.model.model.clone(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> AgentConfig {
        AgentConfig {
            name: "Helper".into(),
            prompt: "Be brief.".into(),
            model: ModelConfig::anthropic("claude-opus-5-5", ApiKey::new("sk-secret-123")),
            limits: Limits::default(),
            manifest_version: 1,
            only_from: None,
        }
    }

    #[test]
    fn a_key_never_prints() {
        let printed = format!("{:?}", config());
        assert!(!printed.contains("sk-secret-123"), "{printed}");
        assert!(printed.contains("redacted"));
    }

    #[test]
    fn the_manifest_discloses_where_text_goes_and_asks_for_little() {
        let m = discordia_protocol::validate_manifest(config().manifest()).unwrap();
        let agent = m.agent.unwrap();
        assert_eq!(agent.provider, "Anthropic");
        assert_eq!(agent.model, "claude-opus-5-5");
        assert!(
            m.permissions
                .iter()
                .all(|p| matches!(p, Permission::SendMessages | Permission::ReadMessageHistory))
        );
        assert!(m.description.contains("Anthropic"));
    }
}
