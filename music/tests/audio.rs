#![cfg(feature = "audio")]

use discordia_music::audio::{
    AudioError, BUFFER_FRAMES, DecodedStream, FRAME_SAMPLES, RATE,
    player::{Clock, Phase, Player},
};
use std::{io::Write, time::Duration};

fn wav(rate: u32, channels: u16, frames: usize, left: i16, right: i16) -> tempfile::NamedTempFile {
    let mut file = tempfile::Builder::new().suffix(".wav").tempfile().unwrap();
    let bytes = (frames * channels as usize * 2) as u32;
    file.write_all(b"RIFF").unwrap();
    file.write_all(&(36 + bytes).to_le_bytes()).unwrap();
    file.write_all(b"WAVEfmt ").unwrap();
    file.write_all(&16u32.to_le_bytes()).unwrap();
    file.write_all(&1u16.to_le_bytes()).unwrap();
    file.write_all(&channels.to_le_bytes()).unwrap();
    file.write_all(&rate.to_le_bytes()).unwrap();
    file.write_all(&(rate * channels as u32 * 2).to_le_bytes())
        .unwrap();
    file.write_all(&(channels * 2).to_le_bytes()).unwrap();
    file.write_all(&16u16.to_le_bytes()).unwrap();
    file.write_all(b"data").unwrap();
    file.write_all(&bytes.to_le_bytes()).unwrap();
    let mut samples = Vec::with_capacity(bytes as usize);
    for _ in 0..frames {
        for channel in 0..channels {
            samples.extend_from_slice(&if channel == 0 { left } else { right }.to_le_bytes());
        }
    }
    file.write_all(&samples).unwrap();
    file.flush().unwrap();
    file
}

#[tokio::test]
async fn stereo_resampling_preserves_channels_duration_and_partial_tail() {
    for rate in [8000u32, 44100, 48000, 96000, 192000] {
        let input = rate as usize / 2 + 37;
        let file = wav(rate, 2, input, 4000, -8000);
        let mut decoder = DecodedStream::file(file.path().into());
        let mut frames = 0;
        let mut energy = [0i64; 2];
        while let Some(frame) = decoder.recv().await {
            let frame = frame.unwrap();
            assert!((1..=FRAME_SAMPLES).contains(&frame.valid_frames));
            assert!(
                frame.samples[frame.valid_frames * 2..]
                    .iter()
                    .all(|v| *v == 0)
            );
            for pair in frame.samples[..frame.valid_frames * 2].as_chunks::<2>().0 {
                energy[0] += pair[0] as i64;
                energy[1] += pair[1] as i64;
            }
            frames += frame.valid_frames;
        }
        decoder.stop().await.unwrap();
        assert_eq!(frames, input * RATE as usize / rate as usize, "rate {rate}");
        assert!(
            (energy[0] / frames as i64 - 4000).abs() < 40,
            "left, rate {rate}"
        );
        assert!(
            (energy[1] / frames as i64 + 8000).abs() < 80,
            "right, rate {rate}"
        );
    }
}

#[tokio::test]
async fn mono_is_duplicated_and_music_is_not_limited_to_soundboard_length() {
    let file = wav(RATE, 1, RATE as usize * 11, 4321, 0);
    let mut decoder = DecodedStream::file(file.path().into());
    let mut frames = 0;
    while let Some(frame) = decoder.recv().await {
        let frame = frame.unwrap();
        frames += frame.valid_frames;
        assert!(
            frame.samples[..frame.valid_frames * 2]
                .as_chunks::<2>()
                .0
                .iter()
                .all(|pair| pair == &[4321, 4321])
        );
    }
    decoder.stop().await.unwrap();
    assert_eq!(frames, RATE as usize * 11);
}

#[tokio::test]
async fn a_slow_or_paused_consumer_has_a_bounded_queue_and_can_cancel() {
    let file = wav(RATE, 2, RATE as usize * 5, 1000, -1000);
    let decoder = DecodedStream::file(file.path().into());
    tokio::time::timeout(Duration::from_secs(3), async {
        while decoder.buffered_frames() != BUFFER_FRAMES {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(decoder.buffered_frames(), BUFFER_FRAMES);
    tokio::time::timeout(Duration::from_secs(1), decoder.stop())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn invalid_file_fails_without_yielding_audio() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(b"not a song").unwrap();
    let mut decoder = DecodedStream::file(file.path().into());
    assert_eq!(
        decoder.recv().await.unwrap().unwrap_err(),
        AudioError::Unsupported
    );
    assert!(decoder.recv().await.is_none());
    assert_eq!(decoder.stop().await.unwrap_err(), AudioError::Unsupported);
}

async fn wait_for_audio(player: &mut Player) -> discordia_music::audio::Frame {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let frame = player.next_frame().await.unwrap();
            if frame.valid_frames > 0 {
                return frame;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn pause_keeps_the_song_position_and_replacement_discards_old_audio() {
    let first = wav(RATE, 2, RATE as usize, 1234, -2345);
    let second = wav(RATE, 2, RATE as usize, -3456, 4567);
    let mut player = Player::default();
    player.load(first.path().into()).await.unwrap();
    assert_eq!(wait_for_audio(&mut player).await.samples[0], 1234);
    let position = player.position_frames();
    player.pause();
    for _ in 0..20 {
        assert!(
            player
                .next_frame()
                .await
                .unwrap()
                .samples
                .iter()
                .all(|v| *v == 0)
        );
    }
    assert_eq!(player.phase(), Phase::Paused);
    assert_eq!(player.position_frames(), position);
    player.resume();
    assert_eq!(wait_for_audio(&mut player).await.samples[0], 1234);
    assert_eq!(player.position_frames(), position + FRAME_SAMPLES as u64);
    player.load(second.path().into()).await.unwrap();
    let fresh = wait_for_audio(&mut player).await;
    assert_eq!(&fresh.samples[..2], &[-3456, 4567]);
    player.stop().await.unwrap();
    assert_eq!(player.phase(), Phase::Idle);
}

#[tokio::test(start_paused = true)]
async fn silence_uses_the_same_clock_and_long_stalls_require_a_restart() {
    let mut clock = Clock::default();
    let mut idle = Player::default();
    assert_eq!(clock.tick().await.unwrap(), 0);
    assert_eq!(idle.next_frame().await.unwrap().valid_frames, 0);
    tokio::time::advance(Duration::from_millis(10)).await;
    assert_eq!(clock.tick().await.unwrap(), FRAME_SAMPLES as u64);
    tokio::time::advance(Duration::from_millis(300)).await;
    assert_eq!(clock.tick().await.unwrap_err(), AudioError::ClockStalled);
}
