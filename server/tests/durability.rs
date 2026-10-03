use std::path::PathBuf;

use dioxusfun_server::media::{DEFAULT_MAX_BYTES, MediaStore};
use dioxusfun_server::protocol::{ChannelKind, Guild, Permission, User};
use dioxusfun_server::state::AppState;
use dioxusfun_server::store::Store;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

struct Fixture {
    dir: PathBuf,
    state: AppState,
    faults: sqlx::SqlitePool,
    owner: User,
    guest: User,
    guild: Guild,
}

impl Fixture {
    async fn new() -> Self {
        let dir =
            std::env::temp_dir().join(format!("discordia-durability-{}", uuid::Uuid::new_v4()));
        let store = Store::open_in(&dir).await.unwrap();
        let faults = SqlitePoolOptions::new()
            .connect_with(SqliteConnectOptions::new().filename(dir.join("discordia.db")))
            .await
            .unwrap();
        let media = MediaStore::open(dir.join("media"), DEFAULT_MAX_BYTES).unwrap();
        let state = AppState::load_or_seed(store, media, Default::default())
            .await
            .unwrap();
        let owner = User {
            pubkey: "owner".into(),
            username: "Owner".into(),
        };
        let guest = User {
            pubkey: "guest".into(),
            username: "Guest".into(),
        };
        let (guild, _, _, _) = state.create_guild("Durable", None, &owner).await.unwrap();
        state.add_member(guild.id, &guest).await.unwrap();
        Self {
            dir,
            state,
            faults,
            owner,
            guest,
            guild,
        }
    }

    async fn fail(&self, operation: &str, table: &str) {
        sqlx::query(&format!(
            "CREATE TRIGGER injected_failure BEFORE {operation} ON {table} BEGIN SELECT RAISE(ABORT, 'injected write failure'); END"
        ))
        .execute(&self.faults)
        .await
        .unwrap();
    }

    async fn recover(&self) {
        sqlx::query("DROP TRIGGER injected_failure")
            .execute(&self.faults)
            .await
            .unwrap();
    }

    async fn reloaded(&self) -> AppState {
        let store = Store::open_in(&self.dir).await.unwrap();
        let media = MediaStore::open(self.dir.join("media"), DEFAULT_MAX_BYTES).unwrap();
        AppState::load_or_seed(store, media, Default::default())
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn failed_message_is_refused_and_retry_survives_restart() {
    let f = Fixture::new().await;
    let channel = f
        .state
        .channels
        .iter()
        .find(|c| c.guild_id == f.guild.id && c.kind == ChannelKind::Text)
        .unwrap()
        .id;
    f.fail("INSERT", "messages").await;
    assert!(
        f.state
            .push_message(channel, f.owner.clone(), "not saved".into(), None, None)
            .await
            .is_err()
    );
    assert!(f.state.history(channel, 50, None).await.is_empty());
    f.recover().await;
    f.state
        .push_message(channel, f.owner.clone(), "saved".into(), None, None)
        .await
        .unwrap();
    assert_eq!(
        f.reloaded().await.history(channel, 50, None).await[0].content,
        "saved"
    );
}

#[tokio::test]
async fn failed_role_update_preserves_permissions_in_memory_and_on_disk() {
    let f = Fixture::new().await;
    let role = f
        .state
        .create_role(f.guild.id, "Reader", None, vec![], &f.owner.pubkey)
        .await
        .unwrap();
    f.state
        .set_member_role(f.guild.id, role.id, &f.guest.pubkey, true, &f.owner.pubkey)
        .await
        .unwrap();
    f.fail("UPDATE", "roles").await;
    assert!(
        f.state
            .update_role(
                f.guild.id,
                role.id,
                "Admin",
                None,
                vec![Permission::ManageGuild],
                &f.owner.pubkey
            )
            .await
            .is_err()
    );
    assert!(
        !f.state
            .has_permission(f.guild.id, &f.guest.pubkey, Permission::ManageGuild)
    );
    assert!(!f.reloaded().await.has_permission(
        f.guild.id,
        &f.guest.pubkey,
        Permission::ManageGuild
    ));
    f.recover().await;
    f.state
        .update_role(
            f.guild.id,
            role.id,
            "Admin",
            None,
            vec![Permission::ManageGuild],
            &f.owner.pubkey,
        )
        .await
        .unwrap();
    assert!(f.reloaded().await.has_permission(
        f.guild.id,
        &f.guest.pubkey,
        Permission::ManageGuild
    ));
}

#[tokio::test]
async fn failed_ban_member_delete_rolls_back_the_ban_and_preserves_membership() {
    let f = Fixture::new().await;
    f.fail("DELETE", "members").await;
    assert!(
        f.state
            .ban_member(f.guild.id, &f.guest.pubkey, &f.owner.pubkey)
            .await
            .is_err()
    );
    assert!(!f.state.is_banned(f.guild.id, &f.guest.pubkey));
    assert!(f.state.is_guild_member(f.guild.id, &f.guest.pubkey));
    let restored = f.reloaded().await;
    assert!(!restored.is_banned(f.guild.id, &f.guest.pubkey));
    assert!(restored.is_guild_member(f.guild.id, &f.guest.pubkey));
    f.recover().await;
    f.state
        .ban_member(f.guild.id, &f.guest.pubkey, &f.owner.pubkey)
        .await
        .unwrap();
    let restored = f.reloaded().await;
    assert!(restored.is_banned(f.guild.id, &f.guest.pubkey));
    assert!(!restored.is_guild_member(f.guild.id, &f.guest.pubkey));
}

#[tokio::test]
async fn failed_role_delete_rolls_back_role_and_assignments() {
    let f = Fixture::new().await;
    let role = f
        .state
        .create_role(
            f.guild.id,
            "Moderator",
            None,
            vec![Permission::ManageGuild],
            &f.owner.pubkey,
        )
        .await
        .unwrap();
    f.state
        .set_member_role(f.guild.id, role.id, &f.guest.pubkey, true, &f.owner.pubkey)
        .await
        .unwrap();
    f.fail("UPDATE", "members").await;
    assert!(
        f.state
            .delete_role(f.guild.id, role.id, &f.owner.pubkey)
            .await
            .is_err()
    );
    assert!(
        f.state
            .has_permission(f.guild.id, &f.guest.pubkey, Permission::ManageGuild)
    );
    assert!(f.reloaded().await.has_permission(
        f.guild.id,
        &f.guest.pubkey,
        Permission::ManageGuild
    ));
}

#[tokio::test]
async fn failed_guild_bundle_creates_neither_rows_nor_memory_state() {
    let f = Fixture::new().await;
    let count = f.state.guilds.len();
    f.fail("INSERT", "channels").await;
    assert!(
        f.state
            .create_guild("Failed", None, &f.owner)
            .await
            .is_err()
    );
    assert_eq!(f.state.guilds.len(), count);
    assert_eq!(f.reloaded().await.guilds.len(), count);
}

#[tokio::test]
async fn failed_profile_and_invite_changes_preserve_previous_values() {
    let f = Fixture::new().await;
    f.fail("INSERT", "profiles").await;
    assert!(
        f.state
            .set_profile(
                &f.owner.pubkey,
                None,
                None,
                Some("unsaved".into()),
                None,
                None
            )
            .await
            .is_err()
    );
    assert!(!f.state.profiles.contains_key(&f.owner.pubkey));
    f.recover().await;
    let invite = f
        .state
        .get_or_create_invite(f.guild.id, false, None, None, &f.owner.pubkey)
        .await
        .unwrap();
    f.fail("INSERT", "invites").await;
    assert!(
        f.state
            .get_or_create_invite(f.guild.id, true, None, None, &f.owner.pubkey)
            .await
            .is_err()
    );
    assert_eq!(f.state.invite_guild(&invite.code), Some(f.guild.id));
    assert_eq!(
        f.reloaded().await.invite_guild(&invite.code),
        Some(f.guild.id)
    );
}

#[tokio::test]
async fn failed_invite_redemption_does_not_use_an_invite_or_admit_a_member() {
    let f = Fixture::new().await;
    let invite = f
        .state
        .get_or_create_invite(f.guild.id, false, None, Some(1), &f.owner.pubkey)
        .await
        .unwrap();
    let newcomer = User {
        pubkey: "new".into(),
        username: "New".into(),
    };
    f.fail("UPDATE", "invites").await;
    assert!(
        f.state
            .join_by_invite(&invite.code, &newcomer)
            .await
            .is_err()
    );
    assert!(!f.state.is_guild_member(f.guild.id, &newcomer.pubkey));
    assert!(
        !f.reloaded()
            .await
            .is_guild_member(f.guild.id, &newcomer.pubkey)
    );
    f.recover().await;
    f.state
        .join_by_invite(&invite.code, &newcomer)
        .await
        .unwrap();
    assert!(
        f.reloaded()
            .await
            .is_guild_member(f.guild.id, &newcomer.pubkey)
    );
    assert!(
        f.state
            .join_by_invite(&invite.code, &f.guest)
            .await
            .is_err()
    );
}
