use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use discordia_bot::{Bot, BotError, BotIdentity};
use discordia_protocol::{
    ClientMessage, Id, Invocation, MAX_BIO_CHARS, MAX_COMMAND_REPLY, MAX_MESSAGE_BYTES, Member,
    Message, Profile, ServerMessage, mentions_name, sanitize_paragraph,
};
use sha2::{Digest, Sha256};
use tokio::task::JoinSet;

use crate::config::{ASK, AgentConfig, Look};
use crate::context;
use crate::harness::{Harness, HarnessError, Prompt, Reply};
use crate::limits::Ledger;
use crate::split::split_reply;

const TICK: Duration = Duration::from_secs(4);
/// History that has not come by then (no read permission, a refused fetch)
/// is answered from the trigger alone.
const HISTORY_WAIT: Duration = Duration::from_secs(4);
const MAX_PARTS: usize = 4;

enum Answer {
    Mention {
        reply_to: Id,
    },
    /// Private: only the person who asked sees it, so no typing in the channel.
    Ask(Box<Invocation>),
}

impl Answer {
    fn public(&self) -> bool {
        matches!(self, Self::Mention { .. })
    }
}

struct Waiting {
    trigger: Message,
    answer: Answer,
    since: Instant,
}

struct Done {
    channel_id: Id,
    guild: Option<Id>,
    answer: Answer,
    result: Result<Reply, HarnessError>,
}

/// Runs one gateway connection until it closes. Reconnecting is the caller's
/// choice; replies still in flight are dropped with the connection.
pub async fn run<H: Harness>(
    mut bot: Bot,
    identity: &BotIdentity,
    config: AgentConfig,
    harness: H,
) -> Result<(), BotError> {
    let mut agent = Agent {
        me: bot.user.pubkey.clone(),
        names: HashSet::from([bot.user.username.clone()]),
        ledger: Ledger::new(config.limits.clone()),
        config,
        harness: Arc::new(harness),
        waiting: HashMap::new(),
        tasks: JoinSet::new(),
        running: HashMap::new(),
        guild_of: HashMap::new(),
    };
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The socket is read the whole time a provider is thinking: a gateway
    // drops a connection whose outbound queue fills (trap 4).
    loop {
        tokio::select! {
            event = bot.next_event() => match event {
                Some(event) => agent.on_event(&mut bot, identity, event).await?,
                None => return Ok(()),
            },
            Some(joined) = agent.tasks.join_next_with_id(), if !agent.tasks.is_empty() => {
                match joined {
                    Ok((task, done)) => {
                        agent.running.remove(&task);
                        agent.deliver(&mut bot, done).await?;
                    }
                    Err(e) => {
                        agent.running.remove(&e.id());
                        agent.ledger.finish(0, None, Utc::now().date_naive());
                        tracing::error!(error = %e, "agent reply task failed");
                    }
                }
            }
            _ = tick.tick() => agent.on_tick(&mut bot).await?,
        }
    }
}

/// Every connect would otherwise re-upload the pictures and broadcast a
/// profile to the whole server.
fn wears(held: Option<&Profile>, look: &Look) -> bool {
    let same_picture = |held: Option<&String>, want: Option<&Vec<u8>>| match (held, want) {
        (None, None) => true,
        (Some(address), Some(bytes)) => {
            let name = format!("media:{}.", hex::encode(Sha256::digest(bytes)));
            address.starts_with(&name)
        }
        _ => false,
    };
    let bio = |b: Option<&String>| {
        b.map(|b| sanitize_paragraph(b, MAX_BIO_CHARS))
            .filter(|b| !b.is_empty())
    };
    same_picture(held.and_then(|p| p.avatar.as_ref()), look.avatar.as_ref())
        && same_picture(held.and_then(|p| p.banner.as_ref()), look.banner.as_ref())
        && bio(held.and_then(|p| p.bio.as_ref())) == bio(look.bio.as_ref())
}

struct Agent<H> {
    config: AgentConfig,
    harness: Arc<H>,
    me: String,
    /// Every name a guild knows this app by: an admin may rename it per guild.
    names: HashSet<String>,
    ledger: Ledger,
    waiting: HashMap<Id, Vec<Waiting>>,
    tasks: JoinSet<Done>,
    /// Channel and whether the answer is public, per running task.
    running: HashMap<tokio::task::Id, (Id, bool)>,
    /// Which guild each channel is in, for that guild's share of the budget.
    guild_of: HashMap<Id, Id>,
}

impl<H: Harness> Agent<H> {
    async fn on_event(
        &mut self,
        bot: &mut Bot,
        identity: &BotIdentity,
        event: ServerMessage,
    ) -> Result<(), BotError> {
        match event {
            ServerMessage::Ready {
                user,
                members,
                channels,
                profiles,
                ..
            } => {
                if let Some(look) = &self.config.look {
                    let held = profiles.iter().find(|p| p.pubkey == user.pubkey);
                    if !wears(held, look) {
                        match bot
                            .set_profile(
                                look.avatar.as_deref(),
                                look.banner.as_deref(),
                                look.bio.as_deref(),
                            )
                            .await
                        {
                            // A picture of another kind must not keep the agent offline.
                            Err(BotError::Image(e)) => {
                                tracing::warn!(error = %e, "agent picture not sent")
                            }
                            other => other?,
                        }
                    }
                }
                self.me = user.pubkey;
                self.names.insert(user.username);
                self.learn(&members);
                self.guild_of
                    .extend(channels.iter().map(|c| (c.id, c.guild_id)));
                bot.register_manifest(identity, &self.config.manifest())
                    .await?;
                bot.register_commands(self.config.commands()).await?;
            }
            ServerMessage::GuildJoined {
                members, channels, ..
            } => {
                self.learn(&members);
                self.guild_of
                    .extend(channels.iter().map(|c| (c.id, c.guild_id)));
            }
            ServerMessage::ChannelCreate(c) | ServerMessage::ChannelUpdate(c) => {
                self.guild_of.insert(c.id, c.guild_id);
            }
            ServerMessage::MemberJoin(m) | ServerMessage::MemberUpdate(m) => {
                self.learn(std::slice::from_ref(&m))
            }
            ServerMessage::MessageCreate(msg) if self.triggered(&msg) => {
                let reply_to = msg.id;
                self.start(bot, msg, Answer::Mention { reply_to }).await?;
            }
            ServerMessage::CommandInvoked(inv) if inv.command == ASK => {
                let question = inv.text("question").unwrap_or("").trim().to_string();
                if self
                    .config
                    .only_from
                    .as_ref()
                    .is_some_and(|o| *o != inv.invoker.pubkey)
                {
                    return bot
                        .respond(&inv, "I only answer the person who runs me.")
                        .await;
                }
                if question.is_empty() {
                    return bot.respond(&inv, "Ask me something.").await;
                }
                let trigger = Message {
                    image_metadata: None,
                    attachment: None,
                    pinned: false,
                    id: inv.id,
                    channel_id: inv.channel_id,
                    author: inv.invoker.clone(),
                    author_bot: false,
                    content: question,
                    image: None,
                    reactions: vec![],
                    reply_to: None,
                    created_at: Utc::now(),
                    edited_at: None,
                };
                self.start(bot, trigger, Answer::Ask(Box::new(inv))).await?;
            }
            ServerMessage::MessageHistory {
                channel_id,
                messages,
            } => {
                for w in self.waiting.remove(&channel_id).unwrap_or_default() {
                    self.dispatch(w, &messages);
                }
            }
            ServerMessage::Error { message } => {
                tracing::warn!(%message, "gateway refused an agent request");
            }
            _ => {}
        }
        Ok(())
    }

    fn learn(&mut self, members: &[Member]) {
        for m in members.iter().filter(|m| m.user.pubkey == self.me) {
            self.names.insert(m.user.username.clone());
        }
    }

    /// Another bot is never answered, or two agents would talk forever.
    fn triggered(&self, msg: &Message) -> bool {
        msg.author.pubkey != self.me
            && !msg.author_bot
            && self
                .config
                .only_from
                .as_ref()
                .is_none_or(|o| *o == msg.author.pubkey)
            && !msg.content.trim().is_empty()
            && (msg
                .reply_to
                .as_ref()
                .is_some_and(|r| r.author_pubkey == self.me)
                || self.names.iter().any(|n| mentions_name(&msg.content, n)))
    }

    async fn start(
        &mut self,
        bot: &mut Bot,
        trigger: Message,
        answer: Answer,
    ) -> Result<(), BotError> {
        let channel_id = trigger.channel_id;
        let today = Utc::now().date_naive();
        if let Err((refusal, notify)) = self.ledger.admit(
            &trigger.author.pubkey,
            self.guild_of.get(&channel_id).copied(),
            Instant::now(),
            today,
        ) {
            // A command press waits on an answer; a mention may go unanswered.
            if notify || !answer.public() {
                self.say(bot, channel_id, &answer, refusal.notice()).await?;
            }
            return Ok(());
        }
        if answer.public() {
            bot.send(&ClientMessage::Typing { channel_id }).await?;
        }
        bot.fetch_messages(channel_id, self.config.limits.context_messages, None)
            .await?;
        self.waiting.entry(channel_id).or_default().push(Waiting {
            trigger,
            answer,
            since: Instant::now(),
        });
        Ok(())
    }

    fn dispatch(&mut self, w: Waiting, history: &[Message]) {
        let prompt = Prompt {
            system: context::system_prompt(&self.config),
            turns: context::build(
                history,
                &w.trigger,
                &self.me,
                self.config.limits.context_tokens,
            ),
        };
        let harness = self.harness.clone();
        let channel_id = w.trigger.channel_id;
        let guild = self.guild_of.get(&channel_id).copied();
        let public = w.answer.public();
        let answer = w.answer;
        let handle = self.tasks.spawn(async move {
            let result = harness.reply(prompt).await;
            Done {
                channel_id,
                guild,
                answer,
                result,
            }
        });
        self.running.insert(handle.id(), (channel_id, public));
    }

    async fn on_tick(&mut self, bot: &mut Bot) -> Result<(), BotError> {
        let now = Instant::now();
        let mut late = Vec::new();
        for list in self.waiting.values_mut() {
            let (old, fresh): (Vec<_>, Vec<_>) = list
                .drain(..)
                .partition(|w| now.duration_since(w.since) >= HISTORY_WAIT);
            *list = fresh;
            late.extend(old);
        }
        self.waiting.retain(|_, list| !list.is_empty());
        for w in late {
            self.dispatch(w, &[]);
        }
        let typing: HashSet<Id> = self
            .running
            .values()
            .filter(|(_, public)| *public)
            .map(|(channel, _)| *channel)
            .chain(
                self.waiting
                    .iter()
                    .filter(|(_, list)| list.iter().any(|w| w.answer.public()))
                    .map(|(channel, _)| *channel),
            )
            .collect();
        for channel_id in typing {
            bot.send(&ClientMessage::Typing { channel_id }).await?;
        }
        Ok(())
    }

    async fn deliver(&mut self, bot: &mut Bot, done: Done) -> Result<(), BotError> {
        let tokens = match &done.result {
            Ok(r) => r.tokens,
            Err(e) => e.tokens(),
        };
        self.ledger
            .finish(tokens, done.guild, Utc::now().date_naive());
        let text = match done.result {
            Ok(r) => r.text,
            Err(HarnessError::Refused { .. }) => "I can't help with that one.".to_string(),
            Err(e) => {
                tracing::warn!(error = %e, "agent provider call failed");
                "I couldn't reach my model just now. Try again in a bit.".to_string()
            }
        };
        self.say(bot, done.channel_id, &done.answer, &text).await
    }

    async fn say(
        &self,
        bot: &mut Bot,
        channel_id: Id,
        answer: &Answer,
        text: &str,
    ) -> Result<(), BotError> {
        match answer {
            Answer::Mention { reply_to } => {
                for (i, part) in split_reply(text, MAX_MESSAGE_BYTES, MAX_PARTS)
                    .iter()
                    .enumerate()
                {
                    if i == 0 {
                        bot.reply_message(channel_id, part, *reply_to).await?;
                    } else {
                        bot.send_message(channel_id, part).await?;
                    }
                }
                Ok(())
            }
            // The cap is in characters; counting bytes keeps under it.
            Answer::Ask(inv) => match split_reply(text, MAX_COMMAND_REPLY, 1).first() {
                Some(part) => bot.respond(inv, part).await,
                None => Ok(()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(avatar: Option<&[u8]>, bio: Option<&str>) -> Profile {
        Profile {
            pubkey: "k".into(),
            avatar: avatar.map(|b| format!("media:{}.png", hex::encode(Sha256::digest(b)))),
            banner: None,
            bio: bio.map(str::to_string),
            status: None,
            custom_status: None,
            hide_guild_owner: false,
            hide_operator: false,
            operator: false,
            updated_at: 0,
        }
    }

    #[test]
    fn a_look_already_worn_is_not_sent_again() {
        let look = Look {
            avatar: Some(b"png".to_vec()),
            banner: None,
            bio: Some("  Helps.  ".into()),
        };
        assert!(wears(Some(&held(Some(b"png"), Some("Helps."))), &look));
        assert!(!wears(Some(&held(Some(b"gif"), Some("Helps."))), &look));
        assert!(!wears(Some(&held(Some(b"png"), None)), &look));
        assert!(!wears(None, &look));
    }

    #[test]
    fn clearing_the_look_clears_what_the_server_holds() {
        let bare = Look::default();
        assert!(wears(None, &bare));
        assert!(wears(Some(&held(None, Some(""))), &bare));
        assert!(!wears(Some(&held(Some(b"png"), None)), &bare));
    }
}
