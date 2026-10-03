use libwebrtc::{
    audio_source::{AudioSourceOptions, native::NativeAudioSource},
    peer_connection_factory::{
        PeerConnectionFactory, RtcConfiguration, native::PeerConnectionFactoryExt,
    },
};

#[tokio::test(flavor = "multi_thread")]
async fn removing_an_audio_track_after_close_returns_an_error_without_aborting() {
    let factory = PeerConnectionFactory::default();
    let peer = factory
        .create_peer_connection(RtcConfiguration::default())
        .unwrap();
    let source = NativeAudioSource::new(AudioSourceOptions::default(), 48_000, 1, 0);
    let track = factory.create_audio_track("close-regression", source);
    let sender = peer.add_track(track.into(), &["voice"]).unwrap();
    peer.close();
    for _ in 0..32 {
        let error = peer.remove_track(sender.clone()).unwrap_err();
        assert!(!error.message.is_empty());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_while_removing_a_track_keeps_the_native_error_path_safe() {
    let factory = PeerConnectionFactory::default();
    for _ in 0..16 {
        let peer = factory
            .create_peer_connection(RtcConfiguration::default())
            .unwrap();
        let source = NativeAudioSource::new(AudioSourceOptions::default(), 48_000, 1, 0);
        let track = factory.create_audio_track("racing-close-regression", source);
        let sender = peer.add_track(track.into(), &["voice"]).unwrap();
        let closing_peer = peer.clone();
        let closing = tokio::task::spawn_blocking(move || closing_peer.close());
        if let Err(error) = peer.remove_track(sender.clone()) {
            assert!(!error.message.is_empty());
        }
        closing.await.unwrap();
        let error = peer.remove_track(sender).unwrap_err();
        assert!(!error.message.is_empty());
    }
}
