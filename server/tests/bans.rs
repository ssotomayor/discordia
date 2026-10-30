use std::path::{Path, PathBuf};

use discordia_server::media::{DEFAULT_MAX_BYTES, MediaStore};
use discordia_server::protocol::{Guild, User};
use discordia_server::state::{AppState, MAX_GUILD_BANS};
use discordia_server::store::Store;
use sqlx::SqlitePool;
use sqlx::sqlite::SqliteConnectOptions;

struct Fixture {
    dir: PathBuf,
    state: AppState,
    owner: User,
    guild: Guild,
}

async fn open(dir: &Path) -> AppState {
    let store = Store::open_in(dir).await.unwrap();
    let media = MediaStore::open(dir.join("media"), DEFAULT_MAX_BYTES).unwrap();
    AppState::load_or_seed(store, media, Default::default())
        .await
        .unwrap()
}

impl Fixture {
    async fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("discordia-bans-{}", uuid::Uuid::new_v4()));
        let state = open(&dir).await;
        let owner = User {
            pubkey: "f".repeat(64),
            username: "Owner".into(),
        };
        let (guild, _, _, _) = state.create_guild("Bans", None, &owner).await.unwrap();
        Self {
            dir,
            state,
            owner,
            guild,
        }
    }

    async fn db(&self) -> SqlitePool {
        SqlitePool::connect_with(
            SqliteConnectOptions::new().filename(self.dir.join("discordia.db")),
        )
        .await
        .unwrap()
    }

    async fn longest(&self, column: &str, table: &str) -> i64 {
        sqlx::query_scalar(&format!(
            "SELECT COALESCE(MAX(length({column})), 0) FROM {table}"
        ))
        .fetch_one(&self.db().await)
        .await
        .unwrap()
    }
}

fn key(n: usize) -> String {
    format!("{n:064x}")
}

#[tokio::test]
async fn a_ban_names_a_key_as_identify_admits_it() {
    let f = Fixture::new().await;
    let gid = f.guild.id;
    for bad in [
        "a".repeat(1_000_000),
        key(0xabc).to_uppercase(),
        "guest".into(),
    ] {
        let err = f
            .state
            .ban_member(gid, &bad, &f.owner.pubkey)
            .await
            .unwrap_err();
        assert!(err.contains("64 lowercase hex"), "got: {err}");
    }
    assert_eq!(f.longest("pubkey", "bans").await, 0);
    f.state
        .ban_member(gid, &key(0xabc), &f.owner.pubkey)
        .await
        .unwrap();
    assert!(f.state.is_banned(gid, &key(0xabc)));

    f.state
        .audit(gid, &f.owner.pubkey, "ban", &"t".repeat(100_000), "")
        .await;
    assert_eq!(f.longest("target", "audit_log").await, 128);
}

#[tokio::test]
async fn a_guild_bans_a_bounded_number_of_keys() {
    let f = Fixture::new().await;
    let gid = f.guild.id.to_string();
    let db = f.db().await;
    let mut tx = db.begin().await.unwrap();
    for n in 0..MAX_GUILD_BANS {
        sqlx::query("INSERT INTO bans (guild_id, pubkey) VALUES (?, ?)")
            .bind(&gid)
            .bind(key(n))
            .execute(&mut *tx)
            .await
            .unwrap();
    }
    tx.commit().await.unwrap();

    let state = open(&f.dir).await;
    let err = state
        .ban_member(f.guild.id, &key(MAX_GUILD_BANS), &f.owner.pubkey)
        .await
        .unwrap_err();
    assert!(err.contains("at most"), "got: {err}");
    state
        .unban_member(f.guild.id, &key(0), &f.owner.pubkey)
        .await
        .unwrap();
    state
        .ban_member(f.guild.id, &key(MAX_GUILD_BANS), &f.owner.pubkey)
        .await
        .unwrap();
}

#[tokio::test]
async fn bans_stored_before_keys_were_checked_are_lifted_by_unban() {
    let f = Fixture::new().await;
    let gid = f.guild.id.to_string();
    let db = f.db().await;
    for pk in [key(0xdef).to_uppercase(), "x".repeat(1_000_000)] {
        sqlx::query("INSERT INTO bans (guild_id, pubkey) VALUES (?, ?)")
            .bind(&gid)
            .bind(pk)
            .execute(&db)
            .await
            .unwrap();
    }
    sqlx::query(
        "INSERT INTO audit_log (guild_id, at_ms, actor_pubkey, action, target, detail)
         VALUES (?, 0, ?, 'ban', ?, '')",
    )
    .bind(&gid)
    .bind(&f.owner.pubkey)
    .bind("x".repeat(1_000_000))
    .execute(&db)
    .await
    .unwrap();

    let state = open(&f.dir).await;
    assert!(
        state.is_banned(f.guild.id, &key(0xdef)),
        "a capitalised ban still holds"
    );
    assert_eq!(f.longest("pubkey", "bans").await, 64);
    assert_eq!(f.longest("target", "audit_log").await, 128);
    state
        .unban_member(f.guild.id, &key(0xdef), &f.owner.pubkey)
        .await
        .unwrap();
    assert!(
        !open(&f.dir).await.is_banned(f.guild.id, &key(0xdef)),
        "the unban outlasts a restart"
    );
}
