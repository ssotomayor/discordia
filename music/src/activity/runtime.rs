use super::{
    BUNDLE, manifest,
    session::{Command, Effect, Session},
};
use crate::probe::MetadataProbe;
use discordia_bot::{
    Bot, BotIdentity,
    protocol::{ActivityInstance, Id, MAX_ACTIVITY_DATA, ServerMessage},
};
use serde_json::json;
use std::{collections::HashMap, error::Error, sync::Arc, time::Duration};
use tokio::task::{AbortHandle, JoinSet};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
const MAX_SESSIONS: usize = 16;
const MAX_PROBES: usize = 4;

pub async fn connect(server: &str, identity: &BotIdentity) -> Result<()> {
    let mut bot = tokio::time::timeout(
        Duration::from_secs(20),
        Bot::connect(server, identity, "Music Lab"),
    )
    .await??;
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match bot.next_event().await {
                Some(ServerMessage::Ready { .. }) => return Ok(()),
                Some(ServerMessage::Error { .. }) | None => {
                    return Err("the host did not accept the music service");
                }
                _ => {}
            }
        }
    })
    .await??;
    bot.register_manifest(identity, &manifest()).await?;
    run(bot, identity.pubkey()).await
}

pub async fn run(mut bot: Bot, app_key: &str) -> Result<()> {
    let probe = Arc::new(MetadataProbe::new()?);
    let mut sessions: HashMap<Id, Session> = HashMap::new();
    let mut jobs = JoinSet::new();
    let mut handles: HashMap<(Id, Id), AbortHandle> = HashMap::new();
    loop {
        tokio::select! {
            event = bot.next_event() => match event {
                None => return Ok(()),
                Some(ServerMessage::ActivityInstances { channel_id, instances }) => {
                    let changed = sync(&mut sessions, channel_id, instances, app_key);
                    for id in changed { send_state(&mut bot, &sessions, id, None, None).await?; }
                }
                Some(ServerMessage::ActivityEvent { instance_id, from, data }) => {
                    let Some(session) = sessions.get_mut(&instance_id) else {
                        send_error(&mut bot, instance_id, &from, None, "This session is unavailable. Only one Music Lab session per channel and 16 per service are supported.").await?;
                        continue;
                    };
                    if data.len() > 4096 {
                        send_error(&mut bot, instance_id, &from, None, "That music command is too large.").await?;
                        continue;
                    }
                    let command = match serde_json::from_str::<Command>(&data) {
                        Ok(command) => command,
                        Err(_) => {
                            send_error(&mut bot, instance_id, &from, None, "Invalid music command.").await?;
                            continue;
                        }
                    };
                    let hello = matches!(command, Command::Hello { .. });
                    let request_id = command.request_id();
                    match session.apply(&from, command, jobs.len() < MAX_PROBES) {
                        Ok(effect) => {
                            if let Effect::Probe { entry_id, source } = effect {
                                let probe = Arc::clone(&probe);
                                let handle = jobs.spawn(async move { (instance_id, entry_id, probe.inspect(source).await) });
                                handles.insert((instance_id, entry_id), handle);
                            }
                            send_state(&mut bot, &sessions, instance_id, hello.then_some(from.as_str()), Some(request_id)).await?;
                        }
                        Err(message) => send_error(&mut bot, instance_id, &from, Some(request_id), message).await?,
                    }
                }
                Some(ServerMessage::BundleWanted { sha256 }) => {
                    if manifest().activity.is_some_and(|a| a.sha256 == sha256) {
                        bot.offer_bundle(BUNDLE.as_bytes()).await?;
                    }
                }
                Some(ServerMessage::Error { .. }) => eprintln!("The host rejected a music service request."),
                Some(_) => {}
            },
            completed = jobs.join_next(), if !jobs.is_empty() => {
                match completed {
                    Some(Ok((instance_id, entry_id, report))) => {
                        handles.remove(&(instance_id, entry_id));
                        if sessions.get_mut(&instance_id).is_some_and(|s| s.complete(entry_id, report)) {
                            send_state(&mut bot, &sessions, instance_id, None, None).await?;
                        }
                    }
                    Some(Err(error)) if error.is_cancelled() => {}
                    Some(Err(_)) => return Err("a music source worker failed".into()),
                    None => {}
                }
            }
        }
        handles.retain(|(instance, entry), handle| {
            if sessions.get(instance).is_some_and(|s| s.pending(*entry)) {
                true
            } else {
                handle.abort();
                false
            }
        });
    }
}

pub fn sync(
    sessions: &mut HashMap<Id, Session>,
    channel: Id,
    instances: Vec<ActivityInstance>,
    app_key: &str,
) -> Vec<Id> {
    sessions.retain(|id, s| {
        s.instance.channel_id != channel
            || instances
                .iter()
                .any(|i| i.id == *id && i.app == app_key && !i.participants.is_empty())
    });
    let mut changed = Vec::new();
    for instance in instances
        .into_iter()
        .filter(|i| i.channel_id == channel && i.app == app_key && !i.participants.is_empty())
    {
        let id = instance.id;
        if let Some(session) = sessions.get_mut(&id) {
            session.update(instance);
        } else {
            if sessions.len() >= MAX_SESSIONS
                || sessions.values().any(|s| s.instance.channel_id == channel)
            {
                continue;
            }
            sessions.insert(id, Session::new(instance));
        }
        changed.push(id);
    }
    changed
}

async fn send_state(
    bot: &mut Bot,
    sessions: &HashMap<Id, Session>,
    instance: Id,
    to: Option<&str>,
    ack: Option<Id>,
) -> Result<()> {
    if let Some(session) = sessions.get(&instance) {
        let data = serde_json::to_string(&session.snapshot(ack))?;
        if data.len() > MAX_ACTIVITY_DATA {
            return Err("music snapshot exceeds the activity frame limit".into());
        }
        bot.activity_send(instance, to, &data).await?;
    }
    Ok(())
}

async fn send_error(
    bot: &mut Bot,
    instance: Id,
    to: &str,
    request_id: Option<Id>,
    message: &str,
) -> Result<()> {
    bot.activity_send(
        instance,
        Some(to),
        &json!({"t":"error", "request_id":request_id, "message":message}).to_string(),
    )
    .await?;
    Ok(())
}
