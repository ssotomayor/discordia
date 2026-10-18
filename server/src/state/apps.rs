use super::{AppState, durable};
use crate::auth::{VerifiedManifest, verify_manifest_event};
use crate::protocol::{
    AppManifest, BotInstall, GuildActivity, Id, Member, Permission, User, bundle_blob,
};
use crate::store::Seat;

/// A throwaway key costs nothing, so manifests from bots no guild installed
/// are capped rather than kept without end.
const MAX_UNINSTALLED_MANIFESTS: usize = 1024;

pub enum ManifestOutcome {
    /// Older than or equal to what this host already holds.
    Stale,
    Recorded {
        changed: Vec<(Id, Seat)>,
    },
}

impl AppState {
    pub fn verified_manifest_for(
        &self,
        bot_pubkey: &str,
        event: &str,
    ) -> Result<VerifiedManifest, String> {
        let v = verify_manifest_event(event)?;
        if v.pubkey != bot_pubkey {
            return Err("that manifest is signed by a different key".into());
        }
        Ok(v)
    }

    pub fn app_manifest_event(&self, bot_pubkey: &str) -> Option<String> {
        self.app_manifests.get(bot_pubkey).map(|m| m.1.clone())
    }

    pub fn guild_managers(&self, guild_id: Id) -> Vec<String> {
        self.guild_member_pubkeys(guild_id)
            .into_iter()
            .filter(|p| self.has_permission(guild_id, p, Permission::ManageGuild))
            .collect()
    }

    /// A newer manifest applies at once where it asks for nothing a guild has
    /// not approved, and narrows the grant with it; otherwise it waits.
    pub async fn register_manifest(
        &self,
        bot_pubkey: &str,
        event: &str,
    ) -> Result<ManifestOutcome, String> {
        let v = self.verified_manifest_for(bot_pubkey, event)?;
        let _write = self.durable_writes.lock().await;
        let held = self.app_manifests.get(bot_pubkey).map(|m| m.0);
        if held.is_none()
            && self.bot_guilds(bot_pubkey).is_empty()
            && self.app_manifests.len() >= MAX_UNINSTALLED_MANIFESTS
        {
            return Err(
                "this host holds as many manifests as it will for apps nobody installed".into(),
            );
        }
        if held.is_some_and(|h| h >= v.manifest.version) {
            return Ok(ManifestOutcome::Stale);
        }
        let mut changed = Vec::new();
        for install in self.bot_guilds(bot_pubkey) {
            if let Some(next) = apply_newer(install, &v.manifest) {
                let seat = self.seat_for(&next);
                changed.push((next, seat));
            }
        }
        durable(
            self.store
                .record_manifest(bot_pubkey, v.manifest.version, &v.event, &changed)
                .await,
            "app manifest",
        )?;
        self.app_manifests
            .insert(bot_pubkey.to_string(), (v.manifest.version, v.event));
        let mut seats = Vec::new();
        for (install, seat) in changed {
            self.apply_seat(&install, &seat);
            seats.push((install.guild_id, seat));
            self.put_install(install);
        }
        Ok(ManifestOutcome::Recorded { changed: seats })
    }

    pub async fn approve_app_update(
        &self,
        guild_id: Id,
        bot_pubkey: &str,
        by_pubkey: &str,
    ) -> Result<Seat, String> {
        let _write = self.durable_writes.lock().await;
        self.require_permission(guild_id, by_pubkey, Permission::ManageGuild)?;
        let mut install = self
            .bot_install(guild_id, bot_pubkey)
            .ok_or("that bot isn't installed here")?;
        let pending = install
            .pending
            .take()
            .ok_or("there is no update to approve")?;
        install.permissions = pending.permissions.clone();
        install.intents = pending.intents.clone();
        install.app = Some(pending);
        let seat = self.seat_for(&install);
        durable(
            self.store.update_bot_install(&install, &seat).await,
            "app update",
        )?;
        self.apply_seat(&install, &seat);
        self.put_install(install);
        Ok(seat)
    }

    /// The member row this install should have. A person's row under the same
    /// key is never taken away; only a bot's.
    pub(super) fn seat_for(&self, install: &BotInstall) -> Seat {
        let current = self
            .members
            .get(&install.guild_id)
            .and_then(|g| g.get(&install.bot_pubkey).map(|m| m.clone()));
        match (install.seated(), current) {
            (true, Some(m)) if m.bot && m.user.username == install.name => Seat::Unchanged,
            (true, Some(mut m)) => {
                m.bot = true;
                m.user.username = install.name.clone();
                Seat::Taken(m)
            }
            (true, None) => Seat::Taken(Member {
                user: User {
                    pubkey: install.bot_pubkey.clone(),
                    username: install.name.clone(),
                },
                guild_id: install.guild_id,
                online: false,
                bot: true,
                roles: Vec::new(),
                xp: 0,
            }),
            (false, Some(m)) if m.bot => Seat::Vacated,
            (false, _) => Seat::Unchanged,
        }
    }

    pub(super) fn apply_seat(&self, install: &BotInstall, seat: &Seat) {
        match seat {
            Seat::Unchanged => {}
            Seat::Taken(member) => {
                self.members
                    .entry(install.guild_id)
                    .or_default()
                    .insert(install.bot_pubkey.clone(), member.clone());
            }
            Seat::Vacated => {
                if let Some(g) = self.members.get(&install.guild_id) {
                    g.remove(&install.bot_pubkey);
                }
            }
        }
    }

    pub fn upload_bundle(
        &self,
        guild_id: Id,
        bot_pubkey: &str,
        data_url: &str,
        by_pubkey: &str,
    ) -> Result<(), String> {
        self.require_permission(guild_id, by_pubkey, Permission::ManageGuild)?;
        let install = self
            .bot_install(guild_id, bot_pubkey)
            .ok_or("that bot isn't installed here")?;
        let wanted: Vec<String> = [&install.app, &install.pending]
            .into_iter()
            .flatten()
            .filter_map(|m| m.activity.as_ref().map(|a| bundle_blob(&a.sha256)))
            .collect();
        if wanted.is_empty() {
            return Err("that app's manifest has no activity".into());
        }
        let stored = self
            .media
            .store_bundle_data_url(data_url)
            .map_err(|e| e.to_string())?;
        let name = stored.strip_prefix("media:").unwrap_or(&stored);
        if !wanted.iter().any(|w| w == name) {
            // Left for the sweep: nothing references it.
            return Err("that file is not the bundle the manifest names".into());
        }
        Ok(())
    }

    /// Bundles some guild approved for this app (or is asked to) that the
    /// host does not hold, so the bot is asked for exactly those.
    pub fn bundles_wanted(&self, bot_pubkey: &str) -> Vec<String> {
        let mut out: Vec<String> = self
            .bot_guilds(bot_pubkey)
            .into_iter()
            .flat_map(|i| [i.app, i.pending])
            .flatten()
            .filter_map(|m| m.activity.map(|a| a.sha256))
            .filter(|sha| {
                self.media
                    .existing(&format!("media:{}", bundle_blob(sha)))
                    .is_none()
            })
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The guilds that can now open it. Kept only if it is a bundle a guild
    /// that installed this app approved, so a bot cannot fill the disk.
    pub fn offer_bundle(&self, bot_pubkey: &str, data_url: &str) -> Result<Vec<Id>, String> {
        let wanted: Vec<(Id, String)> = self
            .bot_guilds(bot_pubkey)
            .into_iter()
            .flat_map(|i| {
                let gid = i.guild_id;
                [i.app, i.pending]
                    .into_iter()
                    .flatten()
                    .filter_map(move |m| m.activity.map(|a| (gid, bundle_blob(&a.sha256))))
            })
            .collect();
        if wanted.is_empty() {
            return Err("no guild here has installed this app's activity".into());
        }
        let stored = self
            .media
            .store_bundle_data_url(data_url)
            .map_err(|e| e.to_string())?;
        let name = stored.strip_prefix("media:").unwrap_or(&stored);
        let mut guilds: Vec<Id> = wanted
            .into_iter()
            .filter(|(_, w)| w == name)
            .map(|(g, _)| g)
            .collect();
        if guilds.is_empty() {
            return Err("that bundle isn't the one any guild approved".into());
        }
        guilds.sort_unstable();
        guilds.dedup();
        Ok(guilds)
    }

    /// Only bundles the host holds, so a member never sees an activity it
    /// cannot open.
    pub fn guild_activities(&self, guild_id: Id) -> Vec<GuildActivity> {
        let mut out: Vec<GuildActivity> = self
            .guild_installs(guild_id)
            .into_iter()
            .filter_map(|i| {
                let app = i.app?;
                let activity = app.activity?;
                self.media
                    .existing(&format!("media:{}", bundle_blob(&activity.sha256)))?;
                Some(GuildActivity {
                    bot_pubkey: i.bot_pubkey,
                    name: i.name,
                    icon: app.icon,
                    sha256: activity.sha256,
                    capabilities: activity.capabilities,
                    max_players: activity.max_players,
                })
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// A member of a guild running that approved bundle, or a manager of one
    /// holding it as a pending update.
    pub fn bundle_available(&self, pubkey: &str, blob: &str) -> bool {
        let names = |m: &Option<AppManifest>| {
            m.as_ref()
                .and_then(|m| m.activity.as_ref())
                .is_some_and(|a| bundle_blob(&a.sha256) == blob)
        };
        // Collected first: the permission check reads `bot_installs` again,
        // and a second read on a shard this loop holds can hang (trap 27).
        let guilds: Vec<(Id, bool, bool)> = self
            .bot_installs
            .iter()
            .flat_map(|g| {
                g.value()
                    .iter()
                    .map(|e| {
                        (
                            e.value().guild_id,
                            names(&e.value().app),
                            names(&e.value().pending),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .filter(|(_, app, pending)| *app || *pending)
            .collect();
        guilds.into_iter().any(|(guild_id, app, pending)| {
            (app && self.is_guild_member(guild_id, pubkey))
                || (pending && self.has_permission(guild_id, pubkey, Permission::ManageGuild))
        })
    }

    fn put_install(&self, install: BotInstall) {
        self.bot_installs
            .entry(install.bot_pubkey.clone())
            .or_default()
            .insert(install.guild_id, install);
    }
}

/// `None` when the install has no pin, or already holds this version or newer.
fn apply_newer(mut install: BotInstall, newer: &AppManifest) -> Option<BotInstall> {
    let pinned = install.app.as_ref()?;
    if newer.version <= pinned.version {
        return None;
    }
    if install
        .pending
        .as_ref()
        .is_some_and(|p| p.version >= newer.version)
    {
        return None;
    }
    if pinned.widens(newer) {
        install.pending = Some(newer.clone());
    } else {
        install
            .permissions
            .retain(|p| newer.permissions.contains(p));
        install.intents.retain(|i| newer.intents.contains(i));
        install.app = Some(newer.clone());
        install.pending = None;
    }
    Some(install)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ActivityCapability, ActivityManifest, Intent};

    fn manifest(
        version: u64,
        perms: Vec<Permission>,
        caps: Vec<ActivityCapability>,
    ) -> AppManifest {
        AppManifest {
            name: "Game".into(),
            description: String::new(),
            icon: "🎮".into(),
            version,
            permissions: perms,
            intents: vec![Intent::GuildMessages],
            commands: Vec::new(),
            activity: Some(ActivityManifest {
                sha256: "a".repeat(64),
                capabilities: caps,
                max_players: 2,
                url: None,
            }),
        }
    }

    fn install(app: Option<AppManifest>) -> BotInstall {
        BotInstall {
            guild_id: Id::new_v4(),
            bot_pubkey: "b".repeat(64),
            name: "Game".into(),
            permissions: vec![Permission::SendMessages, Permission::AddReactions],
            intents: vec![Intent::GuildMessages],
            may_call: Vec::new(),
            app,
            pending: None,
        }
    }

    #[test]
    fn a_narrower_update_applies_and_shrinks_the_grant() {
        let v1 = manifest(
            1,
            vec![Permission::SendMessages, Permission::AddReactions],
            vec![],
        );
        let v2 = manifest(2, vec![Permission::SendMessages], vec![]);
        let next = apply_newer(install(Some(v1)), &v2).unwrap();
        assert_eq!(next.app.as_ref().unwrap().version, 2);
        assert_eq!(next.permissions, vec![Permission::SendMessages]);
        assert!(next.pending.is_none());
    }

    #[test]
    fn a_wider_update_waits_and_changes_nothing_granted() {
        let v1 = manifest(1, vec![Permission::SendMessages], vec![]);
        let v2 = manifest(
            2,
            vec![Permission::SendMessages],
            vec![ActivityCapability::MessageSend],
        );
        let before = install(Some(v1));
        let next = apply_newer(before.clone(), &v2).unwrap();
        assert_eq!(next.app, before.app);
        assert_eq!(next.permissions, before.permissions);
        assert_eq!(next.pending.unwrap().version, 2);
    }

    #[test]
    fn an_unpinned_or_older_manifest_changes_nothing() {
        let v2 = manifest(2, vec![], vec![]);
        assert!(apply_newer(install(None), &v2).is_none());
        let v1 = manifest(1, vec![], vec![]);
        assert!(apply_newer(install(Some(v2)), &v1).is_none());
    }

    #[test]
    fn a_new_bundle_alone_is_not_wider() {
        let v1 = manifest(1, vec![], vec![ActivityCapability::UserRead]);
        let mut v2 = manifest(2, vec![], vec![ActivityCapability::UserRead]);
        v2.activity.as_mut().unwrap().sha256 = "c".repeat(64);
        assert!(!v1.widens(&v2));
        v2.activity.as_mut().unwrap().max_players = 4;
        assert!(v1.widens(&v2));
    }
}
