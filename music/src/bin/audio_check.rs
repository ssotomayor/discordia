use clap::Parser;
use discordia_music::audio::{DecodedStream, RATE};
use serde_json::json;
use std::{error::Error, path::PathBuf};

#[derive(Parser)]
#[command(about = "Check native stereo music decoding. Does not play, publish or upload the file.")]
struct Args {
    file: PathBuf,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut stream = DecodedStream::file(Args::parse().file);
    let mut frames = 0u64;
    let mut peak = [0u16; 2];
    let mut error = None;
    while let Some(result) = stream.recv().await {
        match result {
            Ok(frame) => {
                frames += frame.valid_frames as u64;
                for pair in frame.samples[..frame.valid_frames * 2].as_chunks::<2>().0 {
                    for channel in 0..2 {
                        peak[channel] = peak[channel].max(pair[channel].unsigned_abs());
                    }
                }
            }
            Err(failure) => {
                error = Some(failure);
                break;
            }
        }
    }
    let stopped = stream.stop().await;
    if let Some(error) = error {
        return Err(error.into());
    }
    stopped?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "sample_rate": RATE, "channels": 2, "sample_frames": frames,
            "duration_ms": frames * 1000 / u64::from(RATE), "peak_left": peak[0], "peak_right": peak[1],
            "published": false,
        }))?
    );
    Ok(())
}
