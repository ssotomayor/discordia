#![allow(dead_code)]

type EngineResult<T> = Result<T, libwebrtc::RtcError>;

#[path = "../../vendor/livekit/src/rtc_engine/peer_transport.rs"]
#[allow(clippy::collapsible_if, clippy::unnecessary_unwrap)]
mod peer_transport;
use crate::livekit_start_bitrate_tests as start_bitrate;
