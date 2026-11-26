mod decode;
pub mod player;

#[cfg(feature = "transport")]
pub mod transport;

use std::{error::Error, fmt, path::PathBuf};
use tokio::{sync::mpsc, task::JoinHandle};

pub const RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
pub const FRAME_SAMPLES: usize = 480;
pub const FRAME_VALUES: usize = FRAME_SAMPLES * CHANNELS;
pub const BUFFER_FRAMES: usize = 50;

#[derive(Clone, Debug)]
pub struct Frame {
    pub samples: [i16; FRAME_VALUES],
    pub valid_frames: usize,
}

impl Frame {
    pub fn silence() -> Self {
        Self {
            samples: [0; FRAME_VALUES],
            valid_frames: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioError {
    Input,
    Unsupported,
    Corrupt,
    ResourceLimit,
    Cancelled,
    Worker,
    ClockStalled,
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Input => "cannot read this regular audio file",
            Self::Unsupported => "unsupported audio format, sample rate or channel layout",
            Self::Corrupt => "the audio is damaged or contains invalid samples",
            Self::ResourceLimit => "the audio exceeds the decoder resource limits",
            Self::Cancelled => "audio decoding was cancelled",
            Self::Worker => "the audio decoder worker failed",
            Self::ClockStalled => "the audio publisher clock stalled; restart the publication",
        })
    }
}

impl Error for AudioError {}

pub struct DecodedStream {
    receiver: mpsc::Receiver<Result<Frame, AudioError>>,
    worker: Option<JoinHandle<Result<(), AudioError>>>,
}

impl DecodedStream {
    pub fn file(path: PathBuf) -> Self {
        let (sender, receiver) = mpsc::channel(BUFFER_FRAMES);
        let worker = tokio::task::spawn_blocking(move || {
            let result = decode::file(&path, &sender);
            if let Err(error) = result
                && error != AudioError::Cancelled
                && sender.blocking_send(Err(error)).is_err()
            {
                return Err(AudioError::Cancelled);
            }
            result
        });
        Self {
            receiver,
            worker: Some(worker),
        }
    }

    pub async fn recv(&mut self) -> Option<Result<Frame, AudioError>> {
        self.receiver.recv().await
    }

    pub(crate) fn try_recv(
        &mut self,
    ) -> Result<Result<Frame, AudioError>, mpsc::error::TryRecvError> {
        self.receiver.try_recv()
    }

    pub fn buffered_frames(&self) -> usize {
        self.receiver.len()
    }

    pub(crate) fn finished(&self) -> bool {
        self.receiver.is_closed()
    }

    pub async fn stop(mut self) -> Result<(), AudioError> {
        self.receiver.close();
        match self
            .worker
            .take()
            .ok_or(AudioError::Worker)?
            .await
            .map_err(|_| AudioError::Worker)?
        {
            Err(AudioError::Cancelled) | Ok(()) => Ok(()),
            Err(error) => Err(error),
        }
    }
}

impl Drop for DecodedStream {
    fn drop(&mut self) {
        self.receiver.close();
    }
}
