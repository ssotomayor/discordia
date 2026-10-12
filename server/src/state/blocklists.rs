use super::{AppState, durable};
use crate::protocol::{AppBlocklist, Id, MAX_GUILD_BLOCKLISTS, Permission, is_pubkey_hex};

impl AppState {
    pub fn guild_blocklists(&self, guild_id: Id) -> (Vec<String>, Vec<AppBlocklist>) {
        let curators = self
            .followed_blocklists
            .get(&guild_id)
            .map(|c| c.clone())
            .unwrap_or_default();
        let lists = curators
            .iter()
            .filter_map(|c| self.app_blocklists.get(c).map(|l| l.clone()))
            .collect();
        (curators, lists)
    }

    /// The curator whose list names `app`, among those the guild follows.
    pub fn blocked_by(&self, guild_id: Id, app: &str) -> Option<String> {
        let (_, lists) = self.guild_blocklists(guild_id);
        lists
            .into_iter()
            .find(|l| l.blocked.iter().any(|k| k == app))
            .map(|l| l.curator)
    }

    pub async fn set_guild_blocklists(
        &self,
        guild_id: Id,
        curators: Vec<String>,
        by_pubkey: &str,
    ) -> Result<(), String> {
        let _write = self.durable_writes.lock().await;
        self.require_permission(guild_id, by_pubkey, Permission::ManageGuild)?;
        let mut curators: Vec<String> = curators
            .into_iter()
            .map(|c| c.to_ascii_lowercase())
            .filter(|c| is_pubkey_hex(c))
            .collect();
        curators.sort_unstable();
        curators.dedup();
        if curators.len() > MAX_GUILD_BLOCKLISTS {
            return Err(format!(
                "a guild follows at most {MAX_GUILD_BLOCKLISTS} blocklists"
            ));
        }
        durable(
            self.store.set_guild_blocklists(guild_id, &curators).await,
            "guild blocklists",
        )?;
        self.followed_blocklists.insert(guild_id, curators);
        Ok(())
    }

    /// The guilds that follow the curator, whose managers must hear of it.
    pub async fn upload_blocklist(&self, event: &str) -> Result<Vec<Id>, String> {
        let list = crate::auth::verify_blocklist_event(event)?;
        let followers: Vec<Id> = self
            .followed_blocklists
            .iter()
            .filter(|e| e.value().contains(&list.curator))
            .map(|e| *e.key())
            .collect();
        if followers.is_empty() {
            return Err("no guild here follows that blocklist".into());
        }
        let held = self.app_blocklists.get(&list.curator).map(|l| l.created_at);
        if held.is_some_and(|at| at >= list.created_at) {
            return Ok(Vec::new());
        }
        let _write = self.durable_writes.lock().await;
        durable(
            self.store.upsert_app_blocklist(&list).await,
            "app blocklist",
        )?;
        self.app_blocklists.insert(list.curator.clone(), list);
        Ok(followers)
    }
}
