use std::{cell::RefCell, rc::Rc, time::Instant};

use dioxus::prelude::*;

use crate::protocol::{Activity, ActivityKind, Id, Member, Profile, User, VoiceState};
use crate::state::AppState;

type Export = Rc<RefCell<Option<Signal<AppState>>>>;

#[cfg(target_os = "windows")]
fn cpu_micros() -> Option<u64> {
    use windows::Win32::{
        Foundation::FILETIME,
        System::Threading::{GetCurrentProcess, GetProcessTimes},
    };
    let (mut created, mut exited, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    // SAFETY: the current-process handle is live and all output pointers refer to FILETIMEs.
    unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    }
    .ok()?;
    let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
    Some((ticks(kernel) + ticks(user)) / 10)
}

#[cfg(not(target_os = "windows"))]
fn cpu_micros() -> Option<u64> {
    None
}

fn benchmark_view(export: Export) -> Element {
    let state = use_signal(|| {
        let mut s = AppState::empty();
        s.selected_guild = Some(Id::nil());
        for n in 0..200 {
            let pk = format!("member-{n:03}");
            s.members.push(Member {
                user: User {
                    pubkey: pk.clone(),
                    username: pk.clone(),
                },
                guild_id: Id::nil(),
                online: true,
                bot: false,
                roles: vec![],
                xp: 0,
            });
            s.profiles.insert(
                pk.clone(),
                Profile {
                    pubkey: pk,
                    avatar: Some("media:benchmark.png".into()),
                    ..Default::default()
                },
            );
        }
        s.emoji_images.insert(
            "benchmark.png".into(),
            format!("data:image/png;base64,{}", "A".repeat(16_384)),
        );
        s.voice_states.push(VoiceState {
            user_pubkey: "member-000".into(),
            guild_id: Id::nil(),
            channel_id: Some(Id::nil()),
            muted: false,
            deafened: false,
            speaking: false,
            camera_on: false,
            screen_sharing: false,
            screen_watching: vec![],
        });
        s.activities.insert(
            "member-000".into(),
            Activity {
                kind: ActivityKind::Playing,
                name: "Factorio".into(),
                details: None,
                state: None,
                started_ms: Some(1000),
                image: None,
            },
        );
        s
    });
    provide_context(state);
    *export.borrow_mut() = Some(state);
    rsx! { super::members::MembersPanel {} }
}

#[tokio::test]
#[ignore = "controlled UI benchmark: 200 members, 200 updates per scenario, seven samples"]
async fn benchmark_selective_ui() {
    for scenario in ["messages", "speaking", "game_details"] {
        let mut samples = Vec::new();
        for _ in 0..7 {
            let export = Rc::new(RefCell::new(None));
            let mut dom = VirtualDom::new_with_props(benchmark_view, export.clone());
            dom.rebuild_in_place();
            dom.render_immediate_to_vec();
            let mut state = export.borrow().unwrap();
            let start = Instant::now();
            let cpu_start = cpu_micros();
            let mut edits = 0;
            for n in 0..200 {
                {
                    let mut s = state.write();
                    match scenario {
                        "messages" => {
                            s.messages.insert(Id::from_u128(n), vec![]);
                        }
                        "speaking" => s.voice_states[0].speaking = n % 2 == 0,
                        _ => {
                            s.activities.get_mut("member-000").unwrap().details =
                                Some(format!("Map {n}"))
                        }
                    }
                }
                edits += dom.render_immediate_to_vec().edits.len();
            }
            let cpu = cpu_start
                .zip(cpu_micros())
                .map(|(a, b)| b.saturating_sub(a));
            samples.push((start.elapsed().as_micros(), edits, cpu));
        }
        println!(
            "UI_BENCH {scenario} {}",
            serde_json::to_string(&samples).unwrap()
        );
    }
}

#[tokio::test]
#[ignore = "isolated-process RAM benchmark; sample process memory at UI_RAM stages"]
async fn benchmark_ram_ui() {
    use std::io::Write;
    let scenario = std::env::var("DISCORDIA_UI_RAM_SCENARIO").unwrap_or_else(|_| "messages".into());
    let export = Rc::new(RefCell::new(None));
    let mut dom = VirtualDom::new_with_props(benchmark_view, export.clone());
    dom.rebuild_in_place();
    dom.render_immediate_to_vec();
    let mut state = export.borrow().unwrap();
    println!("UI_RAM mounted");
    std::io::stdout().flush().unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    for n in 0..200 {
        {
            let mut s = state.write();
            match scenario.as_str() {
                "messages" => {
                    s.messages.insert(Id::from_u128(n), vec![]);
                }
                "speaking" => s.voice_states[0].speaking = n % 2 == 0,
                "game_details" => {
                    s.activities.get_mut("member-000").unwrap().details = Some(format!("Map {n}"))
                }
                _ => panic!("unknown RAM scenario"),
            }
        }
        dom.render_immediate_to_vec();
    }
    println!("UI_RAM finished");
    std::io::stdout().flush().unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
}

#[tokio::test]
async fn selective_avatar_handles_blob_arrival_and_person_changes() {
    let export: Export = Rc::new(RefCell::new(None));
    let mut dom = VirtualDom::new_with_props(
        |export: Export| {
            let state = use_signal(AppState::empty);
            provide_context(state);
            *export.borrow_mut() = Some(state);
            let pubkey = state
                .read()
                .profile_card
                .clone()
                .unwrap_or_else(|| "alice".into());
            rsx! { super::profiles::Avatar { pubkey, name: "Alice", size: "w-7 h-7" } }
        },
        export.clone(),
    );
    dom.rebuild_in_place();
    dom.render_immediate_to_vec();
    let mut state = export.borrow().unwrap();
    for _ in 0..20 {
        state.write().messages.insert(Id::new_v4(), vec![]);
        assert!(dom.render_immediate_to_vec().edits.is_empty());
    }
    state.write().profiles.insert(
        "alice".into(),
        Profile {
            pubkey: "alice".into(),
            avatar: Some("media:alice.png".into()),
            ..Default::default()
        },
    );
    assert!(dom.render_immediate_to_vec().edits.is_empty());
    state
        .write()
        .emoji_images
        .insert("alice.png".into(), "data:image/png;base64,YQ==".into());
    assert!(!dom.render_immediate_to_vec().edits.is_empty());
    state.write().profile_card = Some("bob".into());
    assert!(!dom.render_immediate_to_vec().edits.is_empty());
    state.write().profiles.insert(
        "bob".into(),
        Profile {
            pubkey: "bob".into(),
            avatar: Some("data:image/png;base64,Yg==".into()),
            ..Default::default()
        },
    );
    assert!(!dom.render_immediate_to_vec().edits.is_empty());
}
