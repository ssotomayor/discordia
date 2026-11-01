//! Agents belong to the account: `agents/<owner>.json` holds what is not
//! secret, and each agent's key and API key are sealed (trap 23) beside it.

use std::path::{Path, PathBuf};

use discordia_agent::{AgentConfig, ApiKey, Limits, ModelConfig, Provider};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Preset {
    Anthropic,
    OpenAi,
    OpenRouter,
    Groq,
    Ollama,
    Custom,
}

impl Preset {
    pub const ALL: [Preset; 6] = [
        Preset::Anthropic,
        Preset::OpenAi,
        Preset::OpenRouter,
        Preset::Groq,
        Preset::Ollama,
        Preset::Custom,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Preset::Anthropic => "anthropic",
            Preset::OpenAi => "open_ai",
            Preset::OpenRouter => "open_router",
            Preset::Groq => "groq",
            Preset::Ollama => "ollama",
            Preset::Custom => "custom",
        }
    }

    pub fn from_id(id: &str) -> Option<Preset> {
        Self::ALL.into_iter().find(|p| p.id() == id)
    }

    pub fn base_url(self) -> &'static str {
        match self {
            Preset::Anthropic => "https://api.anthropic.com",
            Preset::OpenAi => "https://api.openai.com/v1",
            Preset::OpenRouter => "https://openrouter.ai/api/v1",
            Preset::Groq => "https://api.groq.com/openai/v1",
            Preset::Ollama => "http://127.0.0.1:11434/v1",
            Preset::Custom => "",
        }
    }

    pub fn provider(self) -> Provider {
        match self {
            Preset::Anthropic => Provider::Anthropic,
            _ => Provider::OpenAiCompatible,
        }
    }

    pub fn needs_key(self) -> bool {
        !matches!(self, Preset::Ollama | Preset::Custom)
    }

    pub fn default_model(self) -> &'static str {
        match self {
            Preset::Anthropic => "claude-opus-5-5",
            Preset::Groq => "openai/gpt-oss-20b",
            _ => "",
        }
    }

    pub fn editable_address(self) -> bool {
        matches!(self, Preset::Ollama | Preset::Custom)
    }

    /// What members are told their messages go to: a company, or the host
    /// the creator typed in.
    pub fn disclosed(self, base_url: &str) -> String {
        match self {
            Preset::Anthropic => "Anthropic".into(),
            Preset::OpenAi => "OpenAI".into(),
            Preset::OpenRouter => "OpenRouter".into(),
            Preset::Groq => "Groq".into(),
            Preset::Ollama | Preset::Custom => url::Url::parse(base_url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_string))
                .filter(|h| !h.is_empty())
                .unwrap_or_else(|| "a custom server".into()),
        }
    }
}

/// Where an agent runs. A self-hosted server's address changes with each
/// launch, so it is resolved while hosting, not stored.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServerRef {
    SelfHost,
    Dial { url: String, label: String },
}

impl ServerRef {
    pub fn label(&self) -> String {
        match self {
            ServerRef::SelfHost => crate::i18n::text("Your hosted server", &[]),
            ServerRef::Dial { label, .. } => label.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentEntry {
    pub pubkey: String,
    pub name: String,
    pub prompt: String,
    pub preset: Preset,
    pub base_url: String,
    pub model: String,
    pub per_member_per_hour: u32,
    pub daily_tokens: u64,
    /// Raised whenever the manifest would read differently, so hosts adopt it.
    pub manifest_version: u64,
    #[serde(default)]
    pub servers: Vec<ServerRef>,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

impl AgentEntry {
    pub fn draft() -> Self {
        let limits = Limits::default();
        Self {
            pubkey: String::new(),
            name: String::new(),
            prompt: String::new(),
            preset: Preset::Anthropic,
            base_url: Preset::Anthropic.base_url().into(),
            model: Preset::Anthropic.default_model().into(),
            per_member_per_hour: limits.per_member_per_hour,
            daily_tokens: limits.daily_tokens,
            manifest_version: 1,
            servers: Vec::new(),
            enabled: true,
        }
    }

    pub fn model_config(&self, key: ApiKey) -> ModelConfig {
        ModelConfig {
            provider: self.preset.provider(),
            provider_label: self.preset.disclosed(&self.base_url),
            base_url: self.base_url.trim().to_string(),
            model: self.model.trim().to_string(),
            api_key: key,
            max_output_tokens: 4096,
        }
    }

    pub fn config(&self, key: ApiKey) -> AgentConfig {
        AgentConfig {
            name: self.name.trim().to_string(),
            prompt: self.prompt.clone(),
            model: self.model_config(key),
            limits: Limits {
                per_member_per_hour: self.per_member_per_hour,
                daily_tokens: self.daily_tokens,
                ..Limits::default()
            },
            manifest_version: self.manifest_version,
        }
    }

    /// The version to save under: one higher when anything a host shows changed.
    pub fn next_version(&self, previous: Option<&AgentEntry>) -> u64 {
        let shown = |e: &AgentEntry| {
            let mut m = e.config(ApiKey::new("")).manifest();
            m.version = 0;
            m
        };
        match previous {
            Some(p) if shown(p) != shown(self) => p.manifest_version + 1,
            Some(p) => p.manifest_version,
            None => self.manifest_version.max(1),
        }
    }

    /// A server agent in the same form a device agent is edited in.
    pub fn from_server(view: &discordia_protocol::ServerAgentView) -> Self {
        let s = &view.spec;
        let anthropic = s.provider == discordia_protocol::AgentProvider::Anthropic;
        let preset = Preset::ALL
            .into_iter()
            .find(|p| {
                *p != Preset::Custom
                    && p.base_url() == s.base_url
                    && (p.provider() == Provider::Anthropic) == anthropic
            })
            .unwrap_or(if anthropic {
                Preset::Anthropic
            } else {
                Preset::Custom
            });
        Self {
            pubkey: view.pubkey.clone(),
            name: s.name.clone(),
            prompt: s.prompt.clone(),
            preset,
            base_url: s.base_url.clone(),
            model: s.model.clone(),
            per_member_per_hour: s.per_member_per_hour,
            daily_tokens: s.daily_tokens,
            manifest_version: 1,
            servers: Vec::new(),
            enabled: true,
        }
    }

    pub fn server_spec(
        &self,
        guilds: Vec<discordia_protocol::Id>,
    ) -> discordia_protocol::ServerAgentSpec {
        discordia_protocol::ServerAgentSpec {
            name: self.name.trim().to_string(),
            prompt: self.prompt.clone(),
            provider: match self.preset.provider() {
                Provider::Anthropic => discordia_protocol::AgentProvider::Anthropic,
                Provider::OpenAiCompatible => discordia_protocol::AgentProvider::OpenAiCompatible,
            },
            provider_label: self.preset.disclosed(&self.base_url),
            base_url: self.base_url.trim().to_string(),
            model: self.model.trim().to_string(),
            per_member_per_hour: self.per_member_per_hour,
            daily_tokens: self.daily_tokens,
            guilds,
        }
    }

    /// Why this cannot be saved yet, or `None`.
    pub fn problem(&self, has_key: bool) -> Option<String> {
        if self.name.trim().is_empty() {
            return Some(crate::i18n::text("Give it a name.", &[]));
        }
        if self.model.trim().is_empty() {
            return Some(crate::i18n::text("Choose a model.", &[]));
        }
        if url::Url::parse(self.base_url.trim()).is_err() {
            return Some(crate::i18n::text("The address must be a full URL.", &[]));
        }
        if self.preset.needs_key() && !has_key {
            return Some(crate::i18n::text("Paste an API key.", &[]));
        }
        None
    }
}

#[derive(Serialize, Deserialize, Default)]
struct Stored {
    version: u32,
    agents: Vec<AgentEntry>,
}

/// The agent's own key and the API key, which never leave this machine.
pub struct Secrets {
    pub identity: Zeroizing<String>,
    pub api_key: Zeroizing<String>,
}

#[derive(Serialize)]
struct SealedOut<'a> {
    identity: &'a str,
    api_key: &'a str,
}

#[derive(Deserialize)]
struct SealedIn {
    identity: String,
    api_key: String,
}

fn list_path(dir: &Path, owner: &str) -> PathBuf {
    dir.join("agents").join(format!("{owner}.json"))
}

fn secrets_path(dir: &Path, owner: &str, agent: &str) -> PathBuf {
    dir.join("agents")
        .join(owner)
        .join(format!("{agent}.sealed"))
}

/// Both keys are lowercase hex (trap 12), so they are safe as file names.
fn safe(key: &str) -> Result<(), String> {
    discordia_protocol::is_canonical_pubkey(key)
        .then_some(())
        .ok_or_else(|| "not a key".to_string())
}

pub fn load_in(dir: &Path, owner: &str) -> Vec<AgentEntry> {
    if safe(owner).is_err() {
        return Vec::new();
    }
    std::fs::read_to_string(list_path(dir, owner))
        .ok()
        .and_then(|s| serde_json::from_str::<Stored>(&s).ok())
        .map(|s| s.agents)
        .unwrap_or_default()
}

pub fn save_in(dir: &Path, owner: &str, agents: &[AgentEntry]) -> Result<(), String> {
    safe(owner)?;
    let stored = Stored {
        version: 1,
        agents: agents.to_vec(),
    };
    let json = serde_json::to_string_pretty(&stored).map_err(|e| e.to_string())?;
    crate::identity::write_private(&list_path(dir, owner), &json)
}

pub fn seal_in(
    dir: &Path,
    owner: &str,
    agent: &str,
    secrets: &Secrets,
    passphrase: &str,
    log_n: u8,
) -> Result<(), String> {
    safe(owner)?;
    safe(agent)?;
    let out = SealedOut {
        identity: &secrets.identity,
        api_key: &secrets.api_key,
    };
    let plain = Zeroizing::new(serde_json::to_vec(&out).map_err(|e| e.to_string())?);
    let sealed = crate::keyvault::seal(&plain, passphrase, log_n)?;
    crate::identity::write_private(&secrets_path(dir, owner, agent), &sealed)
}

pub fn unseal_in(
    dir: &Path,
    owner: &str,
    agent: &str,
    passphrase: &str,
) -> Result<Secrets, String> {
    safe(owner)?;
    safe(agent)?;
    let sealed = std::fs::read_to_string(secrets_path(dir, owner, agent))
        .map_err(|e| format!("this agent's keys are missing: {e}"))?;
    let plain = crate::keyvault::unseal(sealed.trim(), passphrase)?;
    let back: SealedIn = serde_json::from_slice(&plain)
        .map_err(|e| format!("this agent's keys are unreadable: {e}"))?;
    Ok(Secrets {
        identity: Zeroizing::new(back.identity),
        api_key: Zeroizing::new(back.api_key),
    })
}

pub fn forget_in(dir: &Path, owner: &str, agent: &str) {
    if safe(owner).is_ok() && safe(agent).is_ok() {
        let _ = std::fs::remove_file(secrets_path(dir, owner, agent));
    }
}

pub fn load(owner: &str) -> Vec<AgentEntry> {
    load_in(&crate::identity::config_dir(), owner)
}

pub fn save(owner: &str, agents: &[AgentEntry]) -> Result<(), String> {
    save_in(&crate::identity::config_dir(), owner, agents)
}

/// Blocking: the keyvault may ask the OS keychain, and sealing runs scrypt.
pub fn seal(owner: &str, agent: &str, secrets: &Secrets) -> Result<(), String> {
    let passphrase = crate::keyvault::passphrase()?;
    seal_in(
        &crate::identity::config_dir(),
        owner,
        agent,
        secrets,
        &passphrase,
        crate::keyvault::LOG_N,
    )
}

/// Blocking, like `seal`.
pub fn unseal(owner: &str, agent: &str) -> Result<Secrets, String> {
    let passphrase = crate::keyvault::passphrase()?;
    unseal_in(&crate::identity::config_dir(), owner, agent, &passphrase)
}

pub fn forget(owner: &str, agent: &str) {
    forget_in(&crate::identity::config_dir(), owner, agent)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("discordia-agents-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    const OWNER: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const AGENT: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    fn entry() -> AgentEntry {
        AgentEntry {
            pubkey: AGENT.into(),
            name: "Helper".into(),
            servers: vec![ServerRef::SelfHost],
            ..AgentEntry::draft()
        }
    }

    #[test]
    fn the_list_round_trips_and_holds_no_secret() {
        let d = dir("list");
        save_in(&d, OWNER, &[entry()]).unwrap();
        assert_eq!(load_in(&d, OWNER), vec![entry()]);
        let raw = std::fs::read_to_string(list_path(&d, OWNER)).unwrap();
        assert!(!raw.contains("api_key"));
        assert!(load_in(&d, "../escape").is_empty());
    }

    #[test]
    fn secrets_are_sealed_and_come_back() {
        let d = dir("secrets");
        let secrets = Secrets {
            identity: Zeroizing::new("b58-secret".into()),
            api_key: Zeroizing::new("sk-ant-123".into()),
        };
        seal_in(&d, OWNER, AGENT, &secrets, "pass", 8).unwrap();
        let raw = std::fs::read_to_string(secrets_path(&d, OWNER, AGENT)).unwrap();
        assert!(!raw.contains("sk-ant-123"));
        let back = unseal_in(&d, OWNER, AGENT, "pass").unwrap();
        assert_eq!(back.api_key.as_str(), "sk-ant-123");
        assert!(unseal_in(&d, OWNER, AGENT, "wrong").is_err());
        forget_in(&d, OWNER, AGENT);
        assert!(unseal_in(&d, OWNER, AGENT, "pass").is_err());
        assert!(seal_in(&d, OWNER, "../x", &secrets, "pass", 8).is_err());
    }

    #[test]
    fn the_version_moves_only_when_hosts_would_show_something_new() {
        let before = entry();
        let mut same = before.clone();
        same.prompt = "different instructions".into();
        same.per_member_per_hour = 3;
        assert_eq!(same.next_version(Some(&before)), 1);
        let mut renamed = before.clone();
        renamed.name = "Helper 2".into();
        assert_eq!(renamed.next_version(Some(&before)), 2);
        let mut moved = before.clone();
        moved.model = "claude-haiku-5-5".into();
        assert_eq!(moved.next_version(Some(&before)), 2);
    }

    #[test]
    fn a_custom_address_is_disclosed_by_its_host() {
        assert_eq!(
            Preset::Ollama.disclosed("http://127.0.0.1:11434/v1"),
            "127.0.0.1"
        );
        assert_eq!(Preset::Custom.disclosed("not a url"), "a custom server");
        assert_eq!(Preset::Anthropic.disclosed(""), "Anthropic");
    }

    #[test]
    fn a_server_agent_edits_in_the_same_form_and_comes_back_unchanged() {
        for preset in Preset::ALL {
            let mut e = AgentEntry::draft();
            e.name = "Helper".into();
            e.preset = preset;
            e.base_url = if preset == Preset::Custom {
                "https://llm.example/v1".into()
            } else {
                preset.base_url().into()
            };
            e.model = "m".into();
            let guild = discordia_protocol::Id::new_v4();
            let view = discordia_protocol::ServerAgentView {
                pubkey: "k".into(),
                spec: e.server_spec(vec![guild]),
                key_hint: String::new(),
                online: false,
                status: String::new(),
            };
            let back = AgentEntry::from_server(&view);
            assert_eq!(back.preset, preset, "{preset:?}");
            assert_eq!(back.server_spec(vec![guild]), view.spec);
        }
    }

    #[test]
    fn groq_is_an_openai_compatible_preset_with_a_key_and_a_model() {
        let p = Preset::from_id("groq").unwrap();
        assert_eq!(p, Preset::Groq);
        assert_eq!(p.provider(), Provider::OpenAiCompatible);
        assert!(p.needs_key());
        assert!(!p.editable_address());
        assert_eq!(p.disclosed(p.base_url()), "Groq");
        let mut e = AgentEntry::draft();
        e.name = "Helper".into();
        e.preset = p;
        e.base_url = p.base_url().into();
        e.model = p.default_model().into();
        assert!(e.problem(true).is_none());
        let config = e.model_config(ApiKey::new("k"));
        assert_eq!(config.base_url, "https://api.groq.com/openai/v1");
        assert_eq!(config.model, "openai/gpt-oss-20b");
    }

    #[test]
    fn a_draft_needs_a_name_a_model_and_a_key() {
        let mut e = AgentEntry::draft();
        assert!(e.problem(true).is_some());
        e.name = "Helper".into();
        assert!(e.problem(false).is_some(), "Anthropic needs a key");
        assert!(e.problem(true).is_none());
        e.preset = Preset::Ollama;
        e.base_url = Preset::Ollama.base_url().into();
        e.model = "llama3".into();
        assert!(e.problem(false).is_none(), "a local model needs none");
    }
}
