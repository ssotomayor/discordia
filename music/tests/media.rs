#![cfg(feature = "transport")]

use discordia_music::audio::{
    FRAME_SAMPLES, Frame, RATE,
    player::Clock,
    transport::{Publisher, music_room_options},
};
use futures_util::StreamExt;
use livekit::{prelude::*, webrtc::audio_stream::native::NativeAudioStream};
use livekit_api::access_token::{AccessToken, VideoGrants};
use std::{f64::consts::TAU, time::Duration};

#[derive(Default, Debug)]
struct Measurement {
    frames: u64,
    power: f64,
    bands: [[f64; 4]; 2],
}

impl Measurement {
    fn add(&mut self, data: &[i16]) {
        for pair in data.as_chunks::<2>().0 {
            for (channel, value) in pair.iter().enumerate() {
                let sample = f64::from(*value) / f64::from(i16::MAX);
                self.power += sample * sample;
                for (band, frequency) in [440.0, 880.0].iter().enumerate() {
                    let phase = TAU * frequency * self.frames as f64 / f64::from(RATE);
                    self.bands[channel][band * 2] += sample * phase.sin();
                    self.bands[channel][band * 2 + 1] += sample * phase.cos();
                }
            }
            self.frames += 1;
        }
    }

    fn band(&self, channel: usize, band: usize) -> f64 {
        self.bands[channel][band * 2].powi(2) + self.bands[channel][band * 2 + 1].powi(2)
    }
}

fn token(room: &str, identity: &str, publish: bool) -> String {
    AccessToken::with_api_key(
        &std::env::var("MUSIC_TEST_API_KEY").expect("test SFU key"),
        &std::env::var("MUSIC_TEST_API_SECRET").expect("test SFU secret"),
    )
    .with_identity(identity)
    .with_grants(VideoGrants {
        room_join: true,
        room: room.into(),
        can_publish: publish,
        can_subscribe: !publish,
        can_publish_data: false,
        can_publish_sources: if publish {
            vec!["microphone".into()]
        } else {
            vec![]
        },
        ..Default::default()
    })
    .to_jwt()
    .unwrap()
}

async fn listener(
    url: String,
    token: String,
    key: [u8; 32],
    ready: tokio::sync::oneshot::Sender<()>,
) -> Measurement {
    let (room, mut events) = Room::connect(&url, &token, music_room_options(key, true))
        .await
        .expect("connect synthetic listener");
    ready.send(()).unwrap();
    let audio = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(RoomEvent::TrackSubscribed {
                track: RemoteTrack::Audio(audio),
                ..
            }) = events.recv().await
            {
                break audio;
            }
        }
    })
    .await
    .expect("music track subscription");
    let mut stream = NativeAudioStream::new(audio.rtc_track(), RATE as i32, 2);
    let mut measurement = Measurement::default();
    let deadline = tokio::time::sleep(Duration::from_secs(6));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => break,
            event = events.recv() => if event.is_none() { break; },
            frame = stream.next() => match frame { Some(frame) => {
                assert_eq!(frame.num_channels, 2);
                measurement.add(&frame.data);
            }, None => break }
        }
    }
    stream.close();
    room.close().await.unwrap();
    measurement
}

#[tokio::test]
#[ignore = "requires an explicitly configured, isolated local SFU; synthetic stereo only"]
async fn encrypted_stereo_reaches_two_listeners_and_rejects_a_wrong_key() {
    let url = std::env::var("MUSIC_TEST_SFU_URL").expect("explicit local test SFU URL required");
    let parsed = url::Url::parse(&url).unwrap();
    assert!(matches!(
        parsed.host_str(),
        Some("127.0.0.1" | "localhost" | "[::1]")
    ));
    let room = format!("music-check-{}", discordia_server::protocol::Id::new_v4());
    let mut listeners = Vec::new();
    for (index, key) in [[42; 32], [42; 32], [43; 32]].into_iter().enumerate() {
        let (ready, wait) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(listener(
            url.clone(),
            token(&room, &format!("listener-{index}"), false),
            key,
            ready,
        ));
        tokio::time::timeout(Duration::from_secs(15), wait)
            .await
            .unwrap()
            .unwrap();
        listeners.push(task);
    }
    let mut publisher = Publisher::connect(&url, &token(&room, "music-service", true), [42; 32])
        .await
        .unwrap();
    let mut clock = Clock::default();
    for index in 0..500 {
        let position = clock.tick().await.unwrap();
        let mut frame = Frame::silence();
        if (100..300).contains(&index) || index >= 350 {
            frame.valid_frames = FRAME_SAMPLES;
            for (offset, pair) in frame.samples.as_chunks_mut::<2>().0.iter_mut().enumerate() {
                for (channel, frequency) in [440.0, 880.0].iter().enumerate() {
                    pair[channel] = (0.2
                        * f64::from(i16::MAX)
                        * (TAU * frequency * (position as f64 + offset as f64) / f64::from(RATE))
                            .sin()) as i16;
                }
            }
        }
        publisher.send(&frame).await.unwrap();
    }
    publisher.close().await.unwrap();
    for (index, task) in listeners.into_iter().enumerate() {
        let measurement = tokio::time::timeout(Duration::from_secs(15), task)
            .await
            .unwrap()
            .unwrap();
        if index < 2 {
            assert!(measurement.frames > RATE as u64);
            assert!(
                measurement.band(0, 0) > measurement.band(0, 1) * 10.0,
                "left stereo separation: {measurement:?}"
            );
            assert!(
                measurement.band(1, 1) > measurement.band(1, 0) * 10.0,
                "right stereo separation: {measurement:?}"
            );
        } else {
            assert!(
                measurement.power / ((measurement.frames.max(1) * 2) as f64) < 0.00001,
                "wrong key decoded audio"
            );
        }
    }
}
