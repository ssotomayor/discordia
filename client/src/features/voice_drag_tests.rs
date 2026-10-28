use super::*;
use dioxus::core::Mutation;

#[test]
fn voice_rows_expose_native_drag_sources_and_destinations() {
    let gid = Id::new_v4();
    let source = Id::new_v4();
    let destination = Id::new_v4();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut dom = VirtualDom::new_with_props(
        move |tx: tokio::sync::mpsc::UnboundedSender<ClientMessage>| {
            let state = use_signal(move || {
                let mut state = AppState::empty();
                state.self_user = Some(crate::protocol::User {
                    pubkey: "owner".into(),
                    username: "Owner".into(),
                });
                state.selected_guild = Some(gid);
                state.guilds.push(
                    serde_json::from_value(serde_json::json!({
                        "id":gid,"name":"Guild","icon":null,"owner_pubkey":"owner"
                    }))
                    .unwrap(),
                );
                for (id, name, position) in
                    [(source, "A source", 0), (destination, "B destination", 1)]
                {
                    state.channels.push(serde_json::from_value(serde_json::json!({
                        "id":id,"guild_id":gid,"name":name,"kind":"voice","topic":null,"position":position
                    })).unwrap());
                }
                state.voice_states.push(serde_json::from_value(serde_json::json!({
                    "user_pubkey":"target","guild_id":gid,"channel_id":source,"muted":false,"deafened":false,"speaking":false
                })).unwrap());
                state
            });
            provide_context(state);
            provide_context(use_signal(crate::settings::ClientSettings::default));
            provide_context(GatewayTx(tx));
            let (voice, _) = tokio::sync::mpsc::unbounded_channel();
            provide_context(crate::features::voice::VoiceTx(voice));
            provide_context(crate::nostr::service::NostrTx::test_channel());
            provide_context(
                crate::identity::Identity::restore_from_private_key("01".repeat(32), "Owner")
                    .unwrap(),
            );
            rsx! { ChannelsColumn {} }
        },
        tx,
    );
    dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
    let initial = dom.rebuild_to_vec().edits;

    let has_text_attribute = |name, value: &str| {
        initial.iter().any(|edit| matches!(edit,
        Mutation::SetAttribute { name: actual, value: dioxus::core::AttributeValue::Text(text), .. }
        if *actual == name && text == value
    ))
    };
    assert!(has_text_attribute("data-voice-pubkey", "target"));
    assert!(has_text_attribute("data-voice-movable", "true"));
    assert!(has_text_attribute("data-voice-from", &source.to_string()));
    assert!(has_text_attribute(
        "data-voice-channel",
        &destination.to_string()
    ));
    assert!(rx.try_recv().is_err());
}
