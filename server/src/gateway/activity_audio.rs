use crate::{
    AppContext,
    protocol::{ActivityAudioRole, Id, ServerMessage},
};
use std::{sync::Arc, time::Duration};

static MINTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(16);

pub struct Request {
    pub connection: u64,
    pub key: String,
    pub is_bot: bool,
    pub instance: Id,
    pub request: Id,
    pub epoch: String,
    pub role: ActivityAudioRole,
}

fn error(ctx: &AppContext, request: &Request, message: String) {
    ctx.state.deliver_to_conn(
        request.connection,
        &ServerMessage::ActivityAudioError {
            instance_id: request.instance,
            request_id: request.request,
            message,
        },
    );
}

pub fn grant(ctx: &Arc<AppContext>, request: Request) {
    if !cfg!(feature = "activity-audio") {
        error(
            ctx,
            &request,
            "activity audio is not enabled on this host".into(),
        );
        return;
    }
    if let Err(message) = ctx.state.authorize_activity_audio(
        &request.key,
        request.is_bot,
        request.instance,
        &request.epoch,
        request.role,
    ) {
        error(ctx, &request, message);
        return;
    }
    let permit = match MINTS.try_acquire() {
        Ok(permit) => permit,
        Err(_) => {
            error(
                ctx,
                &request,
                "audio credentials are busy; try again shortly".into(),
            );
            return;
        }
    };
    let (cfg, revision) = ctx.livekit.route();
    let (host, peer) = ctx.state.connection_voice_address(request.connection);
    let urls = cfg.urls_for_client(host.as_deref(), peer);
    let token_cfg = cfg.clone();
    let (instance, epoch, key, connection, role) = (
        request.instance,
        request.epoch.clone(),
        request.key.clone(),
        request.connection,
        request.role,
    );
    let ctx = Arc::clone(ctx);
    tokio::spawn(async move {
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                crate::livekit_activity::mint(&token_cfg, instance, &epoch, &key, connection, role)
            }),
        )
        .await;
        if ctx.state.is_closing(request.connection) {
            return;
        }
        if ctx.livekit.route_revision() != revision {
            error(
                &ctx,
                &request,
                "the media route changed; request a fresh audio credential".into(),
            );
            return;
        }
        if let Err(message) = ctx.state.authorize_activity_audio(
            &request.key,
            request.is_bot,
            request.instance,
            &request.epoch,
            request.role,
        ) {
            error(&ctx, &request, message);
            return;
        }
        let token = match result {
            Ok(Ok(Ok(token))) => token,
            Ok(Ok(Err(message))) => {
                error(&ctx, &request, message);
                return;
            }
            _ => {
                error(
                    &ctx,
                    &request,
                    "the activity audio credential could not be prepared in time".into(),
                );
                return;
            }
        };
        let Some((url, alternatives)) = urls.split_first() else {
            error(
                &ctx,
                &request,
                "no activity audio route is available".into(),
            );
            return;
        };
        ctx.state.deliver_to_conn(
            request.connection,
            &ServerMessage::ActivityAudioToken {
                instance_id: request.instance,
                request_id: request.request,
                epoch: request.epoch,
                role: request.role,
                livekit_url: url.clone(),
                livekit_urls: alternatives.to_vec(),
                token: token.into(),
                expires_in_secs: crate::protocol::activity_audio::TOKEN_TTL.as_secs(),
                ice_servers: cfg.ice_servers(),
            },
        );
    });
}
