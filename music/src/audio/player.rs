use super::{AudioError, DecodedStream, FRAME_SAMPLES, Frame};
use serde::Serialize;
use std::{path::PathBuf, time::Duration};
use tokio::{
    sync::mpsc::error::TryRecvError,
    time::{Instant, Interval, MissedTickBehavior},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Idle,
    Buffering,
    Playing,
    Paused,
    Ended,
    Failed,
}

pub struct Player {
    stream: Option<DecodedStream>,
    phase: Phase,
    consumed_frames: u64,
    underruns: u64,
}

impl Default for Player {
    fn default() -> Self {
        Self {
            stream: None,
            phase: Phase::Idle,
            consumed_frames: 0,
            underruns: 0,
        }
    }
}

impl Player {
    pub fn phase(&self) -> Phase {
        self.phase
    }
    pub fn position_frames(&self) -> u64 {
        self.consumed_frames
    }
    pub fn underruns(&self) -> u64 {
        self.underruns
    }

    pub async fn load(&mut self, path: PathBuf) -> Result<(), AudioError> {
        self.stop().await?;
        self.stream = Some(DecodedStream::file(path));
        self.phase = Phase::Buffering;
        self.consumed_frames = 0;
        self.underruns = 0;
        Ok(())
    }

    pub fn pause(&mut self) {
        if matches!(self.phase, Phase::Playing | Phase::Buffering) {
            self.phase = Phase::Paused;
        }
    }

    pub fn resume(&mut self) {
        if self.phase == Phase::Paused {
            self.phase = Phase::Buffering;
        }
    }

    pub async fn stop(&mut self) -> Result<(), AudioError> {
        self.phase = Phase::Idle;
        if let Some(stream) = self.stream.take() {
            // A discarded track's decode error must not block selecting the next track.
            if let Err(AudioError::Worker) = stream.stop().await {
                return Err(AudioError::Worker);
            }
        }
        Ok(())
    }

    pub async fn next_frame(&mut self) -> Result<Frame, AudioError> {
        if matches!(
            self.phase,
            Phase::Paused | Phase::Idle | Phase::Ended | Phase::Failed
        ) {
            return Ok(Frame::silence());
        }
        let Some(stream) = self.stream.as_mut() else {
            self.phase = Phase::Failed;
            return Err(AudioError::Worker);
        };
        if self.phase == Phase::Buffering && stream.buffered_frames() < 3 && !stream.finished() {
            return Ok(Frame::silence());
        }
        match stream.try_recv() {
            Ok(Ok(frame)) => {
                self.phase = Phase::Playing;
                self.consumed_frames += frame.valid_frames as u64;
                Ok(frame)
            }
            Err(TryRecvError::Empty) => {
                self.underruns += 1;
                self.phase = Phase::Buffering;
                Ok(Frame::silence())
            }
            Err(TryRecvError::Disconnected) => {
                if let Some(stream) = self.stream.take() {
                    stream.stop().await?;
                }
                self.phase = Phase::Ended;
                Ok(Frame::silence())
            }
            Ok(Err(error)) => {
                self.phase = Phase::Failed;
                if let Some(stream) = self.stream.take() {
                    match stream.stop().await {
                        Ok(()) => {}
                        Err(stopped) if stopped == error => {}
                        Err(stopped) => return Err(stopped),
                    }
                }
                Err(error)
            }
        }
    }
}

pub struct Clock {
    interval: Interval,
    frames: u64,
}

impl Default for Clock {
    fn default() -> Self {
        let mut interval = tokio::time::interval(Duration::from_millis(10));
        interval.set_missed_tick_behavior(MissedTickBehavior::Burst);
        Self {
            interval,
            frames: 0,
        }
    }
}

impl Clock {
    pub async fn tick(&mut self) -> Result<u64, AudioError> {
        let due = self.interval.tick().await;
        if Instant::now().duration_since(due) > Duration::from_millis(250) {
            return Err(AudioError::ClockStalled);
        }
        let position = self.frames;
        self.frames += FRAME_SAMPLES as u64;
        Ok(position)
    }
}
