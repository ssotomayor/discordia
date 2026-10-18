use std::collections::HashMap;

use super::AppState;
use crate::protocol::{
    ActivityInstance, ActivityParticipant, ActivityRole, BUILTIN_ACTIVITIES, Id, MAX_ACTIVITY_DATA,
    MAX_INSTANCE_PARTICIPANTS, MAX_INSTANCES_PER_CHANNEL, bundle_blob,
};

/// Channels whose instance list changed, so each goes out once.
pub type Touched = Vec<Id>;

#[derive(Default)]
pub struct Instances(std::sync::Mutex<HashMap<Id, ActivityInstance>>);

impl Instances {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Id, ActivityInstance>> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl AppState {
    pub fn instances_in(&self, channel_id: Id) -> Vec<ActivityInstance> {
        let mut out: Vec<ActivityInstance> = self
            .activity_instances
            .lock()
            .values()
            .filter(|i| i.channel_id == channel_id)
            .cloned()
            .collect();
        out.sort_by_key(|i| i.id);
        out
    }

    pub fn instances_visible_in_guild(&self, guild_id: Id, pubkey: &str) -> Vec<ActivityInstance> {
        if !self.is_guild_member(guild_id, pubkey) {
            return Vec::new();
        }
        let all: Vec<ActivityInstance> = self.activity_instances.lock().values().cloned().collect();
        let mut out: Vec<ActivityInstance> = all
            .into_iter()
            .filter(|i| {
                self.channel(i.channel_id)
                    .is_some_and(|c| c.guild_id == guild_id && self.can_see_channel(pubkey, &c))
            })
            .collect();
        out.sort_by_key(|i| i.id);
        out
    }

    pub fn instance(&self, id: Id) -> Option<ActivityInstance> {
        self.activity_instances.lock().get(&id).cloned()
    }

    /// The bot of an app instance, which hears it as its backend would.
    pub fn instance_bot(instance: &ActivityInstance) -> Option<&str> {
        (!instance.app.starts_with("builtin:")).then_some(instance.app.as_str())
    }

    fn may_use_channel(
        &self,
        pubkey: &str,
        channel_id: Id,
    ) -> Result<crate::protocol::Channel, String> {
        let channel = self.channel(channel_id).ok_or("no such channel")?;
        if !self.is_guild_member(channel.guild_id, pubkey)
            || !self.can_see_channel(pubkey, &channel)
        {
            return Err("you can't see that channel".into());
        }
        Ok(channel)
    }

    /// A built-in sizes itself from the protocol; an app only from the
    /// manifest its guild approved, and only once the host holds its bundle.
    fn max_players_for(&self, guild_id: Id, app: &str) -> Result<u8, String> {
        if let Some(id) = app.strip_prefix("builtin:") {
            return BUILTIN_ACTIVITIES
                .iter()
                .find(|(b, _)| *b == id)
                .map(|(_, n)| *n)
                .ok_or_else(|| "no such activity".into());
        }
        let activity = self
            .bot_install(guild_id, app)
            .and_then(|i| i.app)
            .and_then(|m| m.activity)
            .ok_or("that app has no activity here")?;
        self.media
            .existing(&format!("media:{}", bundle_blob(&activity.sha256)))
            .ok_or("this host doesn't hold that activity yet")?;
        Ok(activity.max_players)
    }

    pub fn start_instance(
        &self,
        pubkey: &str,
        channel_id: Id,
        app: &str,
    ) -> Result<(ActivityInstance, Touched), String> {
        let channel = self.may_use_channel(pubkey, channel_id)?;
        let max_players = self.max_players_for(channel.guild_id, app)?;
        let mut all = self.activity_instances.lock();
        if all.values().filter(|i| i.channel_id == channel_id).count() >= MAX_INSTANCES_PER_CHANNEL
        {
            return Err("this channel already has as many activities running as it can".into());
        }
        let mut touched = leave_all(&mut all, pubkey);
        let instance = ActivityInstance {
            id: Id::new_v4(),
            channel_id,
            app: app.to_string(),
            max_players,
            participants: vec![ActivityParticipant {
                pubkey: pubkey.to_string(),
                role: ActivityRole::Player,
            }],
        };
        all.insert(instance.id, instance.clone());
        touched.push(channel_id);
        Ok((instance, dedup(touched)))
    }

    pub fn join_instance(
        &self,
        pubkey: &str,
        instance_id: Id,
        role: ActivityRole,
    ) -> Result<Touched, String> {
        let channel_id = self
            .instance(instance_id)
            .ok_or("that activity has ended")?
            .channel_id;
        self.may_use_channel(pubkey, channel_id)?;
        let mut all = self.activity_instances.lock();
        let current = all.get(&instance_id).ok_or("that activity has ended")?;
        let already = current.role_of(pubkey);
        if already == Some(role) {
            return Ok(Vec::new());
        }
        if role == ActivityRole::Player && current.players() >= current.max_players as usize {
            return Err("that game is full — join as a spectator".into());
        }
        if already.is_none() && current.participants.len() >= MAX_INSTANCE_PARTICIPANTS {
            return Err("that activity is full".into());
        }
        let mut touched = if already.is_none() {
            leave_all(&mut all, pubkey)
        } else {
            Vec::new()
        };
        let instance = all.get_mut(&instance_id).ok_or("that activity has ended")?;
        match instance
            .participants
            .iter_mut()
            .find(|p| p.pubkey == pubkey)
        {
            Some(p) => p.role = role,
            None => instance.participants.push(ActivityParticipant {
                pubkey: pubkey.to_string(),
                role,
            }),
        }
        touched.push(channel_id);
        Ok(dedup(touched))
    }

    pub fn leave_instance(&self, pubkey: &str, instance_id: Id) -> Touched {
        let mut all = self.activity_instances.lock();
        let Some(instance) = all.get_mut(&instance_id) else {
            return Vec::new();
        };
        let channel_id = instance.channel_id;
        let before = instance.participants.len();
        instance.participants.retain(|p| p.pubkey != pubkey);
        if instance.participants.len() == before {
            return Vec::new();
        }
        if instance.participants.is_empty() {
            all.remove(&instance_id);
        }
        vec![channel_id]
    }

    pub fn leave_all_instances(&self, pubkey: &str) -> Touched {
        dedup(leave_all(&mut self.activity_instances.lock(), pubkey))
    }

    /// Who receives a frame. Spectators watch; they never move.
    pub fn instance_send(
        &self,
        pubkey: &str,
        instance_id: Id,
        to: Option<&str>,
        data: &str,
    ) -> Result<Vec<String>, String> {
        if data.len() > MAX_ACTIVITY_DATA {
            return Err("that activity message is too large".into());
        }
        let instance = self
            .instance(instance_id)
            .ok_or("that activity has ended")?;
        let bot = Self::instance_bot(&instance);
        if bot == Some(pubkey) {
            let guild = self.channel(instance.channel_id).map(|c| c.guild_id);
            if guild.and_then(|g| self.bot_install(g, pubkey)).is_none() {
                return Err("that app isn't installed here any more".into());
            }
        } else {
            self.may_use_channel(pubkey, instance.channel_id)?;
            if instance.role_of(pubkey) != Some(ActivityRole::Player) {
                return Err("only players move".into());
            }
        }
        let mut everyone: Vec<String> = instance
            .participants
            .iter()
            .map(|p| p.pubkey.clone())
            .collect();
        everyone.extend(bot.map(str::to_string));
        match to {
            Some(target) if everyone.iter().any(|p| p == target) && target != pubkey => {
                Ok(vec![target.to_string()])
            }
            Some(_) => Err("that person isn't in this activity".into()),
            None => Ok(everyone.into_iter().filter(|p| p != pubkey).collect()),
        }
    }

    /// Whoever is no longer a member, or can no longer see the channel, is out;
    /// an instance whose channel or app is gone ends. Run after any removal.
    pub fn prune_instances(&self) -> Touched {
        let snapshot: Vec<ActivityInstance> =
            self.activity_instances.lock().values().cloned().collect();
        let mut touched = Vec::new();
        let mut verdicts: Vec<(Id, Option<Vec<String>>)> = Vec::new();
        for instance in snapshot {
            let Some(channel) = self.channel(instance.channel_id) else {
                verdicts.push((instance.id, None));
                continue;
            };
            let app_gone = Self::instance_bot(&instance)
                .is_some_and(|bot| self.bot_install(channel.guild_id, bot).is_none());
            if app_gone {
                verdicts.push((instance.id, None));
                touched.push(channel.id);
                continue;
            }
            let out: Vec<String> = instance
                .participants
                .iter()
                .filter(|p| {
                    !self.is_guild_member(channel.guild_id, &p.pubkey)
                        || !self.can_see_channel(&p.pubkey, &channel)
                })
                .map(|p| p.pubkey.clone())
                .collect();
            if !out.is_empty() {
                verdicts.push((instance.id, Some(out)));
                touched.push(channel.id);
            }
        }
        let mut all = self.activity_instances.lock();
        for (id, out) in verdicts {
            match out {
                None => {
                    all.remove(&id);
                }
                Some(out) => {
                    if let Some(i) = all.get_mut(&id) {
                        i.participants.retain(|p| !out.contains(&p.pubkey));
                        if i.participants.is_empty() {
                            all.remove(&id);
                        }
                    }
                }
            }
        }
        dedup(touched)
    }

    pub fn end_channel_instances(&self, channel_id: Id) -> bool {
        let mut all = self.activity_instances.lock();
        let before = all.len();
        all.retain(|_, i| i.channel_id != channel_id);
        all.len() != before
    }

    /// Whoever lost sight of the channel is taken out of what runs in it.
    pub fn drop_from_channel_instances(&self, channel_id: Id, lost: &[String]) -> bool {
        let mut all = self.activity_instances.lock();
        let mut changed = false;
        for instance in all.values_mut().filter(|i| i.channel_id == channel_id) {
            let before = instance.participants.len();
            instance.participants.retain(|p| !lost.contains(&p.pubkey));
            changed |= instance.participants.len() != before;
        }
        all.retain(|_, i| !i.participants.is_empty());
        changed
    }
}

fn leave_all(all: &mut HashMap<Id, ActivityInstance>, pubkey: &str) -> Touched {
    let mut touched = Vec::new();
    for instance in all.values_mut() {
        let before = instance.participants.len();
        instance.participants.retain(|p| p.pubkey != pubkey);
        if instance.participants.len() != before {
            touched.push(instance.channel_id);
        }
    }
    all.retain(|_, i| !i.participants.is_empty());
    touched
}

fn dedup(mut ids: Touched) -> Touched {
    ids.sort_unstable();
    ids.dedup();
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instance(players: &[(&str, ActivityRole)]) -> ActivityInstance {
        ActivityInstance {
            id: Id::new_v4(),
            channel_id: Id::new_v4(),
            app: "builtin:rps".into(),
            max_players: 2,
            participants: players
                .iter()
                .map(|(p, r)| ActivityParticipant {
                    pubkey: p.to_string(),
                    role: *r,
                })
                .collect(),
        }
    }

    #[test]
    fn leaving_hands_the_host_on_and_the_last_out_ends_it() {
        let mut all = HashMap::new();
        let i = instance(&[("a", ActivityRole::Player), ("b", ActivityRole::Spectator)]);
        let id = i.id;
        all.insert(id, i);
        assert_eq!(leave_all(&mut all, "a").len(), 1);
        assert_eq!(all[&id].host(), Some("b"));
        leave_all(&mut all, "b");
        assert!(all.is_empty());
    }
}
