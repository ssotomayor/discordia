use std::{cell::RefCell, path::Path, rc::Rc};

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use zeroize::Zeroizing;

use crate::{identity::Identity, protocol::Id, state::AppState};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Conversation {
    Dm {
        peer: String,
    },
    Channel {
        origin: String,
        guild: Id,
        channel: Id,
    },
}

impl Conversation {
    pub fn of(state: &AppState, channel: Id) -> Option<Self> {
        if let Some(dm) = state.dm_of(channel) {
            return Some(Self::Dm {
                peer: dm.other_pubkey.clone(),
            });
        }
        let metadata = state.channels.iter().find(|c| c.id == channel)?;
        Some(Self::Channel {
            origin: state.server_origin.clone()?,
            guild: metadata.guild_id,
            channel,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SavedMessage {
    pub conversation: Conversation,
    pub channel_name: String,
    pub guild_name: String,
    pub id: Id,
    pub author: String,
    pub content: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub image: Option<String>,
    pub attachment: Option<crate::protocol::Attachment>,
}

impl SavedMessage {
    pub fn capture(state: &AppState, message: &crate::protocol::Message) -> Option<Self> {
        let conversation = Conversation::of(state, message.channel_id)?;
        let channel = state.channels.iter().find(|c| c.id == message.channel_id);
        let guild = channel.and_then(|c| state.guilds.iter().find(|g| g.id == c.guild_id));
        Some(Self {
            conversation,
            channel_name: channel.map(|c| c.name.clone()).unwrap_or_default(),
            guild_name: guild.map(|g| g.name.clone()).unwrap_or_default(),
            id: message.id,
            author: message.author.pubkey.clone(),
            content: message.content.clone(),
            created_at: message.created_at,
            image: message
                .image
                .as_ref()
                .map(|image| state.media_src(image).unwrap_or(image).to_owned()),
            attachment: message.attachment.clone(),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Stored {
    version: u32,
    owner: String,
    messages: Vec<SavedMessage>,
}

fn load(path: &Path, owner: &str, key: &str) -> Result<Stored, String> {
    let sealed = match std::fs::read_to_string(path) {
        Ok(sealed) => sealed,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Stored {
                version: 1,
                owner: owner.to_owned(),
                messages: Vec::new(),
            });
        }
        Err(error) => return Err(error.to_string()),
    };
    let plain = crate::keyvault::unseal(&sealed, key)?;
    let stored: Stored = serde_json::from_slice(&plain).map_err(|error| error.to_string())?;
    if stored.version != 1 || stored.owner != owner {
        return Err("Invalid saved-message owner or version".into());
    }
    Ok(stored)
}

fn save(path: &Path, stored: &Stored, key: &str) -> Result<(), String> {
    let plain = Zeroizing::new(serde_json::to_vec(stored).map_err(|error| error.to_string())?);
    let sealed = crate::keyvault::seal(&plain, key, crate::keyvault::LOG_N)?;
    crate::identity::write_private(path, &sealed)
}

fn toggle(stored: &mut Stored, message: SavedMessage) {
    if let Some(index) = stored
        .messages
        .iter()
        .position(|m| m.conversation == message.conversation && m.id == message.id)
    {
        stored.messages.remove(index);
    } else {
        stored.messages.insert(0, message);
    }
}

#[derive(Default)]
pub struct State {
    pub messages: Vec<SavedMessage>,
    pub loaded: bool,
    pub busy: bool,
    pub error: bool,
}

enum Command {
    Toggle(Box<SavedMessage>),
    Remove(Conversation, Id),
    Reload,
}

#[derive(Clone)]
pub struct Bookmarks {
    pub state: Signal<State>,
    commands: mpsc::UnboundedSender<Command>,
}

impl Bookmarks {
    pub fn contains(&self, conversation: &Conversation, id: Id) -> bool {
        self.state
            .read()
            .messages
            .iter()
            .any(|m| &m.conversation == conversation && m.id == id)
    }

    fn send(&self, command: Command) {
        if let Err(error) = self.commands.send(command) {
            tracing::warn!(%error, "Saved-message worker is unavailable");
        }
    }

    pub fn toggle(&self, message: SavedMessage) {
        self.send(Command::Toggle(Box::new(message)));
    }
    pub fn remove(&self, conversation: Conversation, id: Id) {
        self.send(Command::Remove(conversation, id));
    }
    pub fn reload(&self) {
        self.send(Command::Reload);
    }
}

pub fn use_bookmarks(identity: &Identity, mut app: Signal<AppState>) {
    let mut state = use_hook(|| Signal::new_in_scope(State::default(), ScopeId::ROOT));
    use_drop(move || state.manually_drop());
    let (tx, rx) = use_hook(|| {
        let (tx, rx) = mpsc::unbounded_channel();
        (tx, Rc::new(RefCell::new(Some(rx))))
    });
    let owner = identity.pubkey.clone();
    let secret = Zeroizing::new(identity.secret_key().secret_bytes());
    let mut digest = Sha256::new();
    digest.update(b"discordia/bookmarks/v1");
    digest.update(secret.as_ref());
    let key = Zeroizing::new(hex::encode(digest.finalize()));
    use_future(move || {
        let receiver = rx.borrow_mut().take();
        let owner = owner.clone();
        let key = key.clone();
        async move {
            let Some(mut receiver) = receiver else { return };
            let path = crate::identity::config_dir()
                .join("bookmarks")
                .join(format!("{owner}.sealed"));
            let mut stored = None::<Stored>;
            let mut command = Command::Reload;
            loop {
                state.write().busy = true;
                let disk_path = path.clone();
                let disk_owner = owner.clone();
                let disk_key = key.clone();
                let current = stored.clone();
                let reloading = matches!(command, Command::Reload);
                let result = tokio::task::spawn_blocking(move || {
                    if matches!(command, Command::Reload) {
                        return load(&disk_path, &disk_owner, &disk_key);
                    }
                    // A failed load must never turn the next save into an overwrite of unreadable data.
                    let mut next = current.ok_or("Saved messages have not loaded")?;
                    match command {
                        Command::Toggle(message) => toggle(&mut next, *message),
                        Command::Remove(conversation, id) => next
                            .messages
                            .retain(|m| m.conversation != conversation || m.id != id),
                        Command::Reload => {}
                    }
                    save(&disk_path, &next, &disk_key)?;
                    Ok::<Stored, String>(next)
                })
                .await;
                match result {
                    Ok(Ok(next)) => {
                        state.write().messages = next.messages.clone();
                        state.write().loaded = true;
                        state.write().error = false;
                        stored = Some(next);
                    }
                    error => {
                        if reloading {
                            stored = None;
                            state.write().loaded = false;
                        }
                        tracing::warn!(?error, "Could not read or write personal saved messages");
                        state.write().error = true;
                        app.write().error_toast = Some(crate::i18n::text(
                            "Could not access your saved messages. Try again.",
                            &[],
                        ));
                    }
                }
                state.write().busy = false;
                let Some(next) = receiver.recv().await else {
                    break;
                };
                command = next;
            }
        }
    });
    use_context_provider(|| Bookmarks {
        state,
        commands: tx,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved(conversation: Conversation, id: Id) -> SavedMessage {
        SavedMessage {
            conversation,
            channel_name: "general".into(),
            guild_name: "Friends".into(),
            id,
            author: "author-key".into(),
            content: "private saved text".into(),
            created_at: chrono::Utc::now(),
            image: Some("data:image/png;base64,example".into()),
            attachment: Some(crate::protocol::Attachment {
                name: "notes.txt".into(),
                media: "media:abc.bin".into(),
                bytes: 17,
                expires_ms: None,
            }),
        }
    }

    fn channel(origin: &str, channel: Id) -> Conversation {
        Conversation::Channel {
            origin: origin.into(),
            guild: Id::nil(),
            channel,
        }
    }

    #[test]
    fn bookmarks_separate_servers_channels_and_private_chats_and_toggle_without_duplicates() {
        let id = Id::new_v4();
        let cid = Id::new_v4();
        let groups = [
            channel("server-a", cid),
            channel("server-b", cid),
            channel("server-a", Id::new_v4()),
            Conversation::Dm {
                peer: "alice".into(),
            },
            Conversation::Dm { peer: "bob".into() },
        ];
        let mut store = Stored {
            version: 1,
            owner: "me".into(),
            messages: vec![],
        };
        for group in &groups {
            toggle(&mut store, saved(group.clone(), id));
        }
        assert_eq!(store.messages.len(), 5);
        toggle(&mut store, saved(groups[0].clone(), id));
        assert_eq!(store.messages.len(), 4);
        assert!(store.messages.iter().all(|m| m.conversation != groups[0]));
        toggle(&mut store, saved(groups[0].clone(), id));
        assert_eq!(store.messages.len(), 5);
    }

    #[test]
    fn encrypted_bookmarks_survive_restart_and_reject_other_accounts_and_corruption() {
        let directory = std::env::temp_dir().join(format!("discordia-bookmarks-{}", Id::new_v4()));
        let path = directory.join("owner.sealed");
        let mut store = load(&path, "owner", "owner-key").unwrap();
        let id = Id::new_v4();
        toggle(
            &mut store,
            saved(
                Conversation::Dm {
                    peer: "alice".into(),
                },
                id,
            ),
        );
        save(&path, &store, "owner-key").unwrap();
        let ciphertext = std::fs::read_to_string(&path).unwrap();
        assert!(!ciphertext.contains("private saved text"));
        assert!(!ciphertext.contains("alice"));
        assert_eq!(load(&path, "owner", "owner-key").unwrap(), store);
        assert!(load(&path, "another-owner", "owner-key").is_err());
        assert!(load(&path, "owner", "another-key").is_err());
        toggle(
            &mut store,
            saved(
                Conversation::Dm {
                    peer: "alice".into(),
                },
                id,
            ),
        );
        save(&path, &store, "owner-key").unwrap();
        assert!(
            load(&path, "owner", "owner-key")
                .unwrap()
                .messages
                .is_empty()
        );
        std::fs::write(&path, "corrupted saved messages").unwrap();
        assert!(load(&path, "owner", "owner-key").is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "corrupted saved messages"
        );
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn private_chat_grouping_follows_the_peer_instead_of_a_local_channel_id() {
        let mut app = AppState::empty();
        let first = Id::new_v4();
        let second = Id::new_v4();
        app.dms.push(crate::state::DmInfo {
            channel_id: first,
            other_pubkey: "alice".into(),
        });
        app.dms.push(crate::state::DmInfo {
            channel_id: second,
            other_pubkey: "alice".into(),
        });
        assert_eq!(
            Conversation::of(&app, first),
            Conversation::of(&app, second)
        );
        assert!(Conversation::of(&app, Id::new_v4()).is_none());
    }
}
