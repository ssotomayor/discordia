//! A personal agent answering NIP-17 DMs with its own key (#458). DMs never
//! touch a gateway (trap 7), so this runs only on its creator's side.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;

use chrono::Utc;
use discordia_agent::{
    AgentConfig, Harness, HarnessError, Http, Ledger, Prompt, Reply, Role, Status, Turn,
    system_prompt,
};
use secp256k1::SecretKey;
use tokio::task::JoinSet;

use crate::nostr::relay::{Filter, RelayEvent, RelayPool};
use crate::nostr::{event, nip17, nip59};

const HISTORY: usize = 20;
/// Older than the loop by more than this is backlog the relays replay on
/// connect, already answered or not worth answering now.
const BACKLOG_SECS: i64 = 60;
/// NIP-59 backdates a wrap by up to two days, so `since` reaches that far.
const WRAP_JITTER_SECS: i64 = 2 * 24 * 3600;

/// One relay pool for the session (trap 33), on the creator's own DM relays,
/// answering only `allowed`. Runs until the task is dropped.
pub async fn answer_dms(
    secret: SecretKey,
    config: AgentConfig,
    allowed: HashMap<String, String>,
    relays: Vec<String>,
    report: impl Fn(Status) + Send + 'static,
) {
    let me = event::xonly_hex(&secret);
    let started = Utc::now().timestamp();
    report(Status::Connecting);
    let (pool, mut events) = RelayPool::connect(relays.clone());
    pool.publish(nip17::dm_relay_list(&secret, &relays, started));
    pool.publish(event::sign_with(
        &secret,
        started,
        0,
        Vec::new(),
        serde_json::json!({
            "name": config.name,
            "about": format!(
                "AI agent. What it reads is sent to {} ({}).",
                config.model.provider_label, config.model.model
            ),
            "bot": true,
        })
        .to_string(),
    ));
    pool.subscribe(
        "agent-dm",
        vec![Filter {
            kinds: Some(vec![nip59::KIND_GIFT_WRAP]),
            p: Some(vec![me.clone()]),
            since: Some(started - WRAP_JITTER_SECS),
            ..Default::default()
        }],
    );

    let harness = Http::new(config.model.clone());
    let mut ledger = Ledger::new(config.limits.clone());
    let mut history: HashMap<String, VecDeque<Turn>> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut replies: JoinSet<(String, String, Result<Reply, HarnessError>)> = JoinSet::new();
    let mut online = false;
    loop {
        tokio::select! {
            ev = events.recv() => {
                let Some(ev) = ev else { return };
                match ev {
                    RelayEvent::Connected(_) if !online => {
                        online = true;
                        report(Status::Online);
                    }
                    RelayEvent::Event(gift) if gift.kind == nip59::KIND_GIFT_WRAP => {
                        let Ok(rumor) = nip59::unwrap(&secret, &gift) else { continue };
                        if rumor.kind != nip17::KIND_CHAT
                            || rumor.pubkey == me
                            || rumor.created_at < started - BACKLOG_SECS
                            || !seen.insert(rumor.id.clone())
                        {
                            continue;
                        }
                        let Ok(msg) = nip17::open_rumor(&me, &rumor) else { continue };
                        let Some(name) = allowed.get(&msg.author) else { continue };
                        let turns = history.entry(msg.author.clone()).or_default();
                        remember(turns, Role::User, format!("{name}: {}", msg.content));
                        let today = Utc::now().date_naive();
                        if let Err((refusal, notify)) = ledger.admit(&msg.author, None, Instant::now(), today) {
                            if notify {
                                send(&pool, &secret, &me, &msg.author, refusal.notice(), &msg.id);
                            }
                            continue;
                        }
                        let prompt = Prompt {
                            system: system_prompt(&config),
                            turns: alternating(turns),
                        };
                        let (h, peer, id) = (harness.clone(), msg.author.clone(), msg.id.clone());
                        replies.spawn(async move { (peer, id, h.reply(prompt).await) });
                    }
                    _ => {}
                }
            }
            Some(Ok((peer, id, result))) = replies.join_next(), if !replies.is_empty() => {
                let tokens = match &result {
                    Ok(r) => r.tokens,
                    Err(e) => e.tokens(),
                };
                ledger.finish(tokens, None, Utc::now().date_naive());
                let text = match result {
                    Ok(r) => r.text,
                    Err(HarnessError::Refused { .. }) => "I can't help with that one.".to_string(),
                    Err(e) => {
                        tracing::warn!(error = %e, "agent DM reply failed");
                        "I couldn't reach my model just now. Try again in a bit.".to_string()
                    }
                };
                if let Some(turns) = history.get_mut(&peer) {
                    remember(turns, Role::Assistant, text.clone());
                }
                send(&pool, &secret, &me, &peer, &text, &id);
            }
        }
    }
}

fn remember(turns: &mut VecDeque<Turn>, role: Role, text: String) {
    turns.push_back(Turn { role, text });
    while turns.len() > HISTORY {
        turns.pop_front();
    }
}

/// Providers want user and assistant to alternate, starting with the user.
fn alternating(turns: &VecDeque<Turn>) -> Vec<Turn> {
    let mut out: Vec<Turn> = Vec::new();
    for t in turns.iter().skip_while(|t| t.role == Role::Assistant) {
        match out.last_mut() {
            Some(prev) if prev.role == t.role => {
                prev.text.push('\n');
                prev.text.push_str(&t.text);
            }
            _ => out.push(t.clone()),
        }
    }
    out
}

/// Only to the peer: the agent has no other device that needs a copy.
fn send(pool: &RelayPool, secret: &SecretKey, me: &str, peer: &str, text: &str, reply_to: &str) {
    let now = Utc::now().timestamp();
    let rumor = nip17::chat_rumor(me, peer, text, Some(reply_to), &[], now);
    match nip59::wrap(secret, peer, &rumor, now) {
        Ok(gift) => pool.publish(gift),
        Err(e) => tracing::warn!(error = %e, "agent DM not wrapped"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_conversation_alternates_and_keeps_only_the_recent_turns() {
        let mut turns = VecDeque::new();
        remember(&mut turns, Role::Assistant, "stray".into());
        for i in 0..30 {
            remember(&mut turns, Role::User, format!("q{i}"));
            remember(&mut turns, Role::User, format!("q{i} again"));
            remember(&mut turns, Role::Assistant, format!("a{i}"));
        }
        assert_eq!(turns.len(), HISTORY);
        let out = alternating(&turns);
        assert_eq!(out[0].role, Role::User);
        assert!(out.windows(2).all(|w| w[0].role != w[1].role));
        assert!(out.last().unwrap().text.starts_with("a29"));
    }

    #[test]
    fn a_reply_opens_only_for_its_recipient_and_answers_the_message() {
        let agent = SecretKey::from_slice(&[7; 32]).unwrap();
        let person = SecretKey::from_slice(&[9; 32]).unwrap();
        let (me, them) = (event::xonly_hex(&agent), event::xonly_hex(&person));
        let now = Utc::now().timestamp();
        let rumor = nip17::chat_rumor(&me, &them, "hello", Some("abc"), &[], now);
        let gift = nip59::wrap(&agent, &them, &rumor, now).unwrap();
        let opened = nip17::open_chat(&person, &them, &gift).unwrap();
        assert_eq!(opened.content, "hello");
        assert_eq!(opened.author, me);
        assert_eq!(opened.reply_to.as_deref(), Some("abc"));
        assert!(
            nip59::unwrap(&agent, &gift).is_err(),
            "no copy addressed to itself"
        );
    }
}
