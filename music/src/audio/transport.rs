use super::{CHANNELS, FRAME_SAMPLES, Frame, RATE};
use livekit::{
    e2ee::{
        E2eeOptions, EncryptionType,
        key_provider::{KeyProvider, KeyProviderOptions},
    },
    options::{AudioEncoding, TrackPublishOptions},
    prelude::*,
    webrtc::{
        audio_frame::AudioFrame,
        audio_source::{AudioSourceOptions, native::NativeAudioSource},
        prelude::RtcAudioSource,
    },
};
use std::{borrow::Cow, error::Error, fmt, time::Duration};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaError {
    Connect,
    Publish,
    Disconnected,
    Capture,
    Close,
}

impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Connect => "could not connect to the dedicated music room",
            Self::Publish => "could not publish the music audio track",
            Self::Disconnected => "the music room disconnected",
            Self::Capture => "the music publisher stopped accepting audio frames",
            Self::Close => "could not close the music room cleanly",
        })
    }
}

impl Error for MediaError {}

pub fn music_room_options(key: [u8; 32], subscribe: bool) -> RoomOptions {
    let mut options = RoomOptions::default();
    options.auto_subscribe = subscribe;
    options.encryption = Some(E2eeOptions {
        encryption_type: EncryptionType::Gcm,
        key_provider: KeyProvider::with_shared_key(KeyProviderOptions::default(), key.to_vec()),
    });
    options
}

pub struct Publisher {
    room: Room,
    source: NativeAudioSource,
    events: tokio::sync::mpsc::UnboundedReceiver<RoomEvent>,
}

impl Publisher {
    pub async fn connect(
        url: &str,
        publish_token: &str,
        music_key: [u8; 32],
    ) -> Result<Self, MediaError> {
        let (room, events) = tokio::time::timeout(
            Duration::from_secs(25),
            Room::connect(url, publish_token, music_room_options(music_key, false)),
        )
        .await
        .map_err(|_| MediaError::Connect)?
        .map_err(|_| MediaError::Connect)?;
        let source = NativeAudioSource::new(
            AudioSourceOptions {
                echo_cancellation: false,
                noise_suppression: false,
                auto_gain_control: false,
            },
            RATE,
            CHANNELS as u32,
            20,
        );
        let track =
            LocalAudioTrack::create_audio_track("music", RtcAudioSource::Native(source.clone()));
        let publication = tokio::time::timeout(
            Duration::from_secs(10),
            room.local_participant().publish_track(
                LocalTrack::Audio(track),
                TrackPublishOptions {
                    // The narrow grant allows this source tag. Samples still come
                    // from the music decoder, never from a microphone device.
                    source: TrackSource::Microphone,
                    audio_encoding: Some(AudioEncoding {
                        max_bitrate: 192_000,
                    }),
                    dtx: false,
                    ..Default::default()
                },
            ),
        )
        .await;
        if !matches!(publication, Ok(Ok(_))) {
            tokio::time::timeout(Duration::from_secs(5), room.close())
                .await
                .map_err(|_| MediaError::Close)?
                .map_err(|_| MediaError::Close)?;
            return Err(MediaError::Publish);
        }
        Ok(Self {
            room,
            source,
            events,
        })
    }

    pub async fn send(&mut self, frame: &Frame) -> Result<(), MediaError> {
        if self.room.connection_state() == ConnectionState::Disconnected {
            return Err(MediaError::Disconnected);
        }
        while let Ok(event) = self.events.try_recv() {
            if matches!(event, RoomEvent::Disconnected { .. }) {
                return Err(MediaError::Disconnected);
            }
        }
        let audio = AudioFrame {
            data: Cow::Borrowed(&frame.samples),
            sample_rate: RATE,
            num_channels: CHANNELS as u32,
            samples_per_channel: FRAME_SAMPLES as u32,
        };
        tokio::time::timeout(Duration::from_secs(1), self.source.capture_frame(&audio))
            .await
            .map_err(|_| MediaError::Capture)?
            .map_err(|_| MediaError::Capture)
    }

    pub async fn close(self) -> Result<(), MediaError> {
        tokio::time::timeout(Duration::from_secs(5), self.room.close())
            .await
            .map_err(|_| MediaError::Close)?
            .map_err(|_| MediaError::Close)
    }
}
