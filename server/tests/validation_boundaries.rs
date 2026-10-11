use dioxusfun_server::media::{DEFAULT_MAX_BYTES, MediaStore};
use dioxusfun_server::protocol::{GuildVisibility, Id, MAX_EMOJIS_PER_GUILD, User};
use dioxusfun_server::state::AppState;
use dioxusfun_server::store::Store;

async fn fixture() -> (AppState, User, Id) {
    let dir = std::env::temp_dir().join(format!("discordia-boundaries-{}", Id::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("store.sqlite")).await.unwrap();
    let media = MediaStore::open(dir.join("media"), DEFAULT_MAX_BYTES).unwrap();
    let state = AppState::load_or_seed(store, media, Default::default())
        .await
        .unwrap();
    let owner = User {
        pubkey: "11".repeat(32),
        username: "owner".into(),
    };
    let (guild, _, _, _) = state.create_guild("limits", None, &owner).await.unwrap();
    (state, owner, guild.id)
}

#[tokio::test]
async fn role_count_accepts_the_last_slot_and_rejects_the_next() {
    let (state, owner, guild) = fixture().await;
    while state.guild_roles(guild).len() < 49 {
        state
            .create_role(guild, "role", None, vec![], &owner.pubkey)
            .await
            .unwrap();
    }
    assert_eq!(state.guild_roles(guild).len(), 49);
    state
        .create_role(guild, "last", None, vec![], &owner.pubkey)
        .await
        .unwrap();
    assert_eq!(state.guild_roles(guild).len(), 50);
    assert!(
        state
            .create_role(guild, "overflow", None, vec![], &owner.pubkey)
            .await
            .unwrap_err()
            .contains("role limit")
    );
    assert_eq!(state.guild_roles(guild).len(), 50);
}

#[tokio::test]
async fn emoji_count_accepts_the_last_slot_and_rejects_the_next() {
    let (state, owner, guild) = fixture().await;
    for n in 0..MAX_EMOJIS_PER_GUILD - 1 {
        state
            .create_emoji(
                guild,
                &format!("emoji_{n}"),
                "pixel.png".into(),
                &owner.pubkey,
            )
            .await
            .unwrap();
    }
    assert_eq!(state.emojis_of(guild).len(), MAX_EMOJIS_PER_GUILD - 1);
    state
        .create_emoji(guild, "last", "pixel.png".into(), &owner.pubkey)
        .await
        .unwrap();
    assert_eq!(state.emojis_of(guild).len(), MAX_EMOJIS_PER_GUILD);
    assert!(
        state
            .create_emoji(guild, "overflow", "pixel.png".into(), &owner.pubkey)
            .await
            .unwrap_err()
            .contains("emoji limit")
    );
    assert_eq!(state.emojis_of(guild).len(), MAX_EMOJIS_PER_GUILD);
}

#[tokio::test]
async fn catalog_pages_are_clamped_with_more_than_five_hundred_public_guilds() {
    let (state, _, guild) = fixture().await;
    let template = state.guilds.get(&guild).unwrap().clone();
    state.guilds.clear();
    for n in 0..501 {
        let mut item = template.clone();
        item.id = Id::new_v4();
        item.name = format!("guild-{n:03}");
        item.visibility = GuildVisibility::Public;
        state.guilds.insert(item.id, item);
    }
    for (limit, expected) in [
        (0, 100),
        (499, 499),
        (500, 500),
        (501, 500),
        (u32::MAX, 500),
    ] {
        let (page, total) = state.guild_catalog_page(0, limit);
        assert_eq!(total, 501);
        assert_eq!(page.len(), expected, "limit {limit}");
    }
    let (first, _) = state.guild_catalog_page(0, 500);
    let (last, total) = state.guild_catalog_page(500, 500);
    assert_eq!(total, 501);
    assert_eq!(last.len(), 1);
    assert!(!first.iter().any(|g| g.id == last[0].id));
    assert!(state.guild_catalog_page(501, 500).0.is_empty());
}
