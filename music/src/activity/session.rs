use crate::{
    probe::{Report, Status},
    source::{Kind, Source},
};
use discordia_bot::protocol::{ActivityInstance, ActivityRole, Id};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

pub const MAX_QUEUE: usize = 8;
const MAX_REQUESTS: usize = 64;

#[derive(Debug, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Hello {
        request_id: Id,
    },
    Add {
        request_id: Id,
        url: String,
    },
    Remove {
        request_id: Id,
        entry_id: Id,
    },
    Move {
        request_id: Id,
        entry_id: Id,
        before: Option<Id>,
    },
    Clear {
        request_id: Id,
    },
}

impl Command {
    pub fn request_id(&self) -> Id {
        match self {
            Self::Hello { request_id }
            | Self::Add { request_id, .. }
            | Self::Remove { request_id, .. }
            | Self::Move { request_id, .. }
            | Self::Clear { request_id } => *request_id,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Entry {
    pub id: Id,
    pub source: Source,
    pub added_by: String,
    pub title: Option<String>,
    pub metadata: Option<Status>,
}

pub struct Session {
    pub instance: ActivityInstance,
    entries: Vec<Entry>,
    recent: VecDeque<(String, Id)>,
    revision: u64,
    epoch: Id,
}

#[derive(Debug)]
pub enum Effect {
    Snapshot,
    Probe { entry_id: Id, source: Source },
}

#[derive(Serialize)]
pub struct Snapshot<'a> {
    t: &'static str,
    revision: u64,
    session_epoch: Id,
    ack: Option<Id>,
    host: Option<&'a str>,
    playback: &'static str,
    queue_limit: usize,
    queue: &'a [Entry],
}

impl Session {
    pub fn new(instance: ActivityInstance) -> Self {
        Self {
            instance,
            entries: Vec::new(),
            recent: VecDeque::new(),
            revision: 0,
            epoch: Id::new_v4(),
        }
    }

    pub fn update(&mut self, instance: ActivityInstance) {
        self.instance = instance;
        self.entries.retain(|e| {
            e.metadata.is_some() || self.instance.role_of(&e.added_by) == Some(ActivityRole::Player)
        });
        self.revision += 1;
    }

    pub fn snapshot(&self, ack: Option<Id>) -> Snapshot<'_> {
        Snapshot {
            t: "state",
            revision: self.revision,
            session_epoch: self.epoch,
            ack,
            host: self.host(),
            playback: "not_connected",
            queue_limit: MAX_QUEUE,
            queue: &self.entries,
        }
    }

    pub fn pending(&self, id: Id) -> bool {
        self.entries
            .iter()
            .any(|e| e.id == id && e.metadata.is_none())
    }

    pub fn apply(
        &mut self,
        from: &str,
        command: Command,
        can_probe: bool,
    ) -> Result<Effect, &'static str> {
        if self.instance.role_of(from) != Some(ActivityRole::Player) {
            return Err("Only participating players can change this queue.");
        }
        let request = command.request_id();
        if self
            .recent
            .iter()
            .any(|(key, seen)| key == from && *seen == request)
        {
            return Ok(Effect::Snapshot);
        }
        let effect = match command {
            Command::Hello { .. } => return Ok(Effect::Snapshot),
            Command::Add { url, .. } => {
                if self.entries.len() >= MAX_QUEUE {
                    return Err("The queue is full.");
                }
                if !can_probe {
                    return Err("The service is busy. Try adding the song again shortly.");
                }
                if self
                    .entries
                    .iter()
                    .filter(|e| e.added_by == from && e.metadata.is_none())
                    .count()
                    >= 2
                {
                    return Err("Wait for your pending songs to finish loading.");
                }
                let source =
                    Source::parse(&url).map_err(|_| "Unsupported or invalid public music link.")?;
                if source.kind() != Kind::Track {
                    return Err(
                        "Playlist and album imports are not connected yet. Add a single track.",
                    );
                }
                let entry_id = Id::new_v4();
                self.entries.push(Entry {
                    id: entry_id,
                    source: source.clone(),
                    added_by: from.into(),
                    title: None,
                    metadata: None,
                });
                Effect::Probe { entry_id, source }
            }
            Command::Remove { entry_id, .. } => {
                let position = self
                    .entries
                    .iter()
                    .position(|e| e.id == entry_id)
                    .ok_or("That song is no longer queued.")?;
                if self.entries[position].added_by != from && self.host() != Some(from) {
                    return Err("You can remove your own songs; the host can remove any song.");
                }
                self.entries.remove(position);
                Effect::Snapshot
            }
            Command::Move {
                entry_id, before, ..
            } => {
                self.require_host(from)?;
                let position = self
                    .entries
                    .iter()
                    .position(|e| e.id == entry_id)
                    .ok_or("That song is no longer queued.")?;
                if before == Some(entry_id) {
                    return Err("Choose a different position.");
                }
                if before.is_some_and(|id| !self.entries.iter().any(|e| e.id == id)) {
                    return Err("The destination song is no longer queued.");
                }
                let entry = self.entries.remove(position);
                let destination = before
                    .and_then(|id| self.entries.iter().position(|e| e.id == id))
                    .unwrap_or(self.entries.len());
                self.entries.insert(destination, entry);
                Effect::Snapshot
            }
            Command::Clear { .. } => {
                self.require_host(from)?;
                self.entries.clear();
                Effect::Snapshot
            }
        };
        self.recent.push_back((from.into(), request));
        if self.recent.len() > MAX_REQUESTS {
            self.recent.pop_front();
        }
        self.revision += 1;
        Ok(effect)
    }

    pub fn complete(&mut self, id: Id, report: Report) -> bool {
        let Some(entry) = self
            .entries
            .iter_mut()
            .find(|e| e.id == id && e.metadata.is_none())
        else {
            return false;
        };
        if entry.source != report.source {
            return false;
        }
        entry.title = report.title.map(|title| title.chars().take(128).collect());
        entry.metadata = Some(report.status);
        self.revision += 1;
        true
    }

    fn require_host(&self, from: &str) -> Result<(), &'static str> {
        if self.host() == Some(from) {
            Ok(())
        } else {
            Err("Only the activity host can do that.")
        }
    }

    fn host(&self) -> Option<&str> {
        self.instance
            .participants
            .iter()
            .find(|p| p.role == ActivityRole::Player)
            .map(|p| p.pubkey.as_str())
    }
}
