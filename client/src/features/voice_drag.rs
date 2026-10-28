use dioxus::prelude::*;

use crate::protocol::{ChannelKind, ClientMessage, Id, Permission};
use crate::state::{AppState, GatewayTx};

#[derive(serde::Deserialize)]
struct DropRequest {
    pubkey: String,
    from_channel_id: Id,
    channel_id: Id,
}

fn allowed(state: &AppState, request: &DropRequest) -> bool {
    let Some(source) = state
        .channels
        .iter()
        .find(|c| c.id == request.from_channel_id)
    else {
        return false;
    };
    source.kind == ChannelKind::Voice
        && state.can(source.guild_id, Permission::MoveMembers)
        && state
            .self_user
            .as_ref()
            .is_some_and(|u| u.pubkey != request.pubkey)
        && !state
            .guilds
            .iter()
            .any(|g| g.id == source.guild_id && g.owner_pubkey == request.pubkey)
        && state.voice_states.iter().any(|v| {
            v.user_pubkey == request.pubkey
                && v.guild_id == source.guild_id
                && v.channel_id == Some(source.id)
        })
        && state.channels.iter().any(|c| {
            c.id == request.channel_id
                && c.id != source.id
                && c.guild_id == source.guild_id
                && c.kind == ChannelKind::Voice
        })
}

pub(super) fn use_drag(
    state: Signal<AppState>,
    gateway: GatewayTx,
) -> (String, EventHandler<MountedEvent>) {
    let root = use_hook(|| format!("voice-drag-{}", Id::new_v4()));
    let encoded = serde_json::to_string(&root).unwrap_or_default();
    let install = encoded.clone();
    let mounted = use_callback(move |_: MountedEvent| {
        let mut eval = document::eval(&format!(
            "({})({install});",
            include_str!("../../assets/voice-drag.js")
        ));
        let gateway = gateway.clone();
        spawn(async move {
            while let Ok(Some(request)) = eval.recv::<Option<DropRequest>>().await {
                if allowed(&state.peek(), &request) {
                    gateway.send(ClientMessage::MoveVoice {
                        user_pubkey: request.pubkey,
                        from_channel_id: request.from_channel_id,
                        channel_id: request.channel_id,
                    });
                }
            }
        });
    });
    use_drop(move || {
        let _ = document::eval(&format!("window.__dxfVoiceDrags?.get({encoded})?.();"));
    });
    (root, mounted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (AppState, DropRequest) {
        let mut state = AppState::empty();
        let guild = Id::new_v4();
        let source = Id::new_v4();
        let destination = Id::new_v4();
        state.self_user = Some(crate::protocol::User {
            pubkey: "owner".into(),
            username: "Owner".into(),
        });
        state.guilds.push(
            serde_json::from_value(serde_json::json!({
                "id":guild,"name":"Guild","icon":null,"owner_pubkey":"owner"
            }))
            .unwrap(),
        );
        for id in [source, destination] {
            state.channels.push(
                serde_json::from_value(serde_json::json!({
                    "id":id,"guild_id":guild,"name":"Voice","kind":"voice","topic":null
                }))
                .unwrap(),
            );
        }
        state.voice_states.push(serde_json::from_value(serde_json::json!({
            "user_pubkey":"target","guild_id":guild,"channel_id":source,"muted":false,"deafened":false,"speaking":false
        })).unwrap());
        (
            state,
            DropRequest {
                pubkey: "target".into(),
                from_channel_id: source,
                channel_id: destination,
            },
        )
    }

    #[test]
    fn drop_rechecks_permissions_and_the_current_voice_source() {
        let (mut state, mut request) = fixture();
        assert!(allowed(&state, &request));
        state.self_user.as_mut().unwrap().pubkey = "moderator".into();
        assert!(!allowed(&state, &request));
        let gid = state.guilds[0].id;
        let role = Id::new_v4();
        state.members.push(serde_json::from_value(serde_json::json!({
            "user":{"pubkey":"moderator","username":"Moderator"},"guild_id":gid,"online":true,"roles":[role]
        })).unwrap());
        state.roles.insert(
            gid,
            vec![serde_json::from_value(serde_json::json!({
            "id":role,"guild_id":gid,"name":"Mover","color":null,"permissions":["move_members"]
        })).unwrap()],
        );
        assert!(allowed(&state, &request));
        state.voice_states[0].channel_id = Some(request.channel_id);
        assert!(!allowed(&state, &request));
        state.voice_states[0].channel_id = Some(request.from_channel_id);
        state.channels[1].guild_id = Id::new_v4();
        assert!(!allowed(&state, &request));
        state.channels[1].guild_id = gid;
        state.channels[1].kind = ChannelKind::Text;
        assert!(!allowed(&state, &request));
        state.channels[1].kind = ChannelKind::Voice;
        request.pubkey = "owner".into();
        assert!(!allowed(&state, &request));
        request.pubkey = "moderator".into();
        assert!(!allowed(&state, &request));
    }
}
