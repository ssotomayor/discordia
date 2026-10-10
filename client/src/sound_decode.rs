use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use base64::Engine as _;
use symphonia::core::codecs::audio::{
    AudioCodecParameters, AudioDecoder, AudioDecoderOptions, CODEC_ID_NULL_AUDIO,
    well_known::CODEC_ID_OPUS,
};
use symphonia::core::errors::Error;
use symphonia::core::formats::{FormatOptions, probe::Hint};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::packet::Packet;

use crate::features::voice::{AudioResampler, RESAMPLER_CHUNK, SAMPLE_RATE};
use crate::protocol::MAX_SOUND_SECS;

pub struct Decoded {
    /// Mono at the voice rate, never longer than `MAX_SOUND_SECS`.
    pub pcm: Vec<f32>,
    pub truncated: bool,
}

/// MIME type and extension, read from the bytes. A file saved out of a browser
/// often has no extension, and one renamed `.mp3` can be Ogg Opus inside.
pub fn sniff(bytes: &[u8]) -> Option<(&'static str, &'static str)> {
    if bytes.starts_with(b"OggS") {
        Some(("audio/ogg", "ogg"))
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE" {
        Some(("audio/wav", "wav"))
    } else if bytes.starts_with(b"ID3")
        || (bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] & 0xE0 == 0xE0)
    {
        Some(("audio/mpeg", "mp3"))
    } else {
        None
    }
}

pub fn decode(bytes: &[u8], ext: Option<&str>) -> Result<Decoded, String> {
    let mss = MediaSourceStream::new(
        Box::new(std::io::Cursor::new(bytes.to_vec())),
        Default::default(),
    );
    let mut hint = Hint::new();
    if let Some(ext) = ext {
        hint.with_extension(ext);
    }
    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|_| "not an MP3, OGG or WAV file this app can read".to_string())?;
    let (track, params) = format
        .tracks()
        .iter()
        .filter_map(|t| t.codec_params.as_ref()?.audio().map(|params| (t, params)))
        .find(|(_, params)| params.codec != CODEC_ID_NULL_AUDIO)
        .ok_or("the file has no audio in it")?;
    let track_id = track.id;
    let rate = params
        .sample_rate
        .ok_or("the file does not say its sample rate")?;
    let mut codec = Codec::for_track(params)?;

    let limit = (MAX_SOUND_SECS * rate as f32) as usize;
    let mut mono: Vec<f32> = Vec::new();
    let mut truncated = false;
    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(Error::ResetRequired) => break,
            Err(e) => return Err(format!("the file is damaged ({e})")),
        };
        if packet.track_id != track_id {
            continue;
        }
        codec.decode_into(&packet, &mut mono)?;
        if mono.len() > limit {
            mono.truncate(limit);
            truncated = true;
            break;
        }
    }
    if mono.is_empty() {
        return Err("the file has no audio in it".into());
    }

    let pcm = match AudioResampler::new(rate, SAMPLE_RATE) {
        Some(mut resampler) => {
            // The resampler holds back a partial chunk; the padding pushes the tail out.
            mono.resize(mono.len() + 2 * RESAMPLER_CHUNK, 0.0);
            let mut out = Vec::new();
            resampler.process_into(&mono, &mut out);
            out.truncate((MAX_SOUND_SECS * SAMPLE_RATE as f32) as usize);
            out
        }
        None => mono,
    };
    Ok(Decoded { pcm, truncated })
}

enum Codec {
    Symphonia(Box<dyn AudioDecoder>),
    Opus(Box<OpusStream>),
}

impl Codec {
    fn for_track(params: &AudioCodecParameters) -> Result<Self, String> {
        if params.codec == CODEC_ID_OPUS {
            return OpusStream::new(params).map(|o| Codec::Opus(Box::new(o)));
        }
        symphonia::default::get_codecs()
            .make_audio_decoder(params, &AudioDecoderOptions::default())
            .map(Codec::Symphonia)
            .map_err(|e| format!("can't decode this audio ({e})"))
    }

    /// A packet that will not decode is skipped, as a player would.
    fn decode_into(&mut self, packet: &Packet, mono: &mut Vec<f32>) -> Result<(), String> {
        let decoder = match self {
            Codec::Opus(opus) => return opus.decode_into(&packet.data, mono),
            Codec::Symphonia(decoder) => decoder,
        };
        let buf = match decoder.decode(packet) {
            Ok(buf) => buf,
            Err(Error::DecodeError(_)) => return Ok(()),
            Err(e) => return Err(format!("the file is damaged ({e})")),
        };
        let channels = buf.spec().channels().count().max(1);
        let mut samples = Vec::<f32>::new();
        buf.copy_to_vec_interleaved(&mut samples);
        mono.extend(
            samples
                .as_slice()
                .chunks(channels)
                .map(|frame| frame.iter().sum::<f32>() / channels as f32),
        );
        Ok(())
    }
}

/// 120 ms at 48 kHz, the longest packet Opus allows.
const OPUS_MAX_FRAME: usize = 5760;

/// What symphonia hands over for Ogg Opus is the RFC 7845 header; pre-skip and
/// output gain are the decoder's job, and a player that ignores them is wrong.
struct OpusStream {
    decoder: opus_rs::OpusDecoder,
    channels: usize,
    skip: usize,
    gain: f32,
    scratch: Vec<f32>,
}

impl OpusStream {
    fn new(params: &AudioCodecParameters) -> Result<Self, String> {
        let head = params
            .extra_data
            .as_deref()
            .filter(|h| h.len() >= 19 && h.starts_with(b"OpusHead"))
            .ok_or("the Opus header is missing")?;
        let channels = head[9] as usize;
        if head[18] != 0 || !(1..=2).contains(&channels) {
            return Err("surround Opus is not supported — export it as mono or stereo".into());
        }
        let skip = u16::from_le_bytes([head[10], head[11]]) as usize;
        let gain_q8 = i16::from_le_bytes([head[16], head[17]]);
        let decoder = opus_rs::OpusDecoder::new(SAMPLE_RATE as i32, channels)
            .map_err(|e| format!("can't decode this audio ({e})"))?;
        let scratch = vec![0.0; OPUS_MAX_FRAME * channels];
        Ok(Self {
            decoder,
            channels,
            skip,
            gain: 10f32.powf(f32::from(gain_q8) / (20.0 * 256.0)),
            scratch,
        })
    }

    fn decode_into(&mut self, packet: &[u8], mono: &mut Vec<f32>) -> Result<(), String> {
        let Ok(n) = self
            .decoder
            .decode(packet, OPUS_MAX_FRAME, &mut self.scratch)
        else {
            return Ok(());
        };
        let n = n.min(OPUS_MAX_FRAME);
        let dropped = self.skip.min(n);
        self.skip -= dropped;
        let (channels, gain) = (self.channels, self.gain);
        mono.extend(
            self.scratch[..n * channels]
                .chunks(channels)
                .skip(dropped)
                .map(|frame| frame.iter().sum::<f32>() / channels as f32 * gain),
        );
        Ok(())
    }
}

pub fn data_url_bytes(data_url: &str) -> Option<Vec<u8>> {
    let (_, payload) = data_url.strip_prefix("data:")?.split_once(";base64,")?;
    base64::engine::general_purpose::STANDARD
        .decode(payload)
        .ok()
}

fn cache() -> &'static Mutex<HashMap<String, Arc<[f32]>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<[f32]>>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Keyed by content address, so a renamed sound keeps its decode and a
/// replaced one cannot be served stale.
pub fn decoded(address: &str, data_url: &str) -> Result<Arc<[f32]>, String> {
    if let Some(hit) = cache().lock().expect("sound cache").get(address) {
        return Ok(hit.clone());
    }
    let bytes = data_url_bytes(data_url).ok_or("the sound did not arrive intact")?;
    let ext = address.rsplit_once('.').map(|(_, e)| e);
    let pcm: Arc<[f32]> = decode(&bytes, ext)?.pcm.into();
    cache()
        .lock()
        .expect("sound cache")
        .insert(address.to_string(), pcm.clone());
    Ok(pcm)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(rate: u32, channels: u16, secs: f32) -> Vec<u8> {
        let frames = (rate as f32 * secs) as usize;
        let data_len = (frames * channels as usize * 2) as u32;
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * channels as u32 * 2).to_le_bytes());
        out.extend_from_slice(&(channels * 2).to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        for i in 0..frames {
            let t = i as f32 / rate as f32;
            let s = ((t * 440.0 * std::f32::consts::TAU).sin() * 0.5 * i16::MAX as f32) as i16;
            for _ in 0..channels {
                out.extend_from_slice(&s.to_le_bytes());
            }
        }
        out
    }

    #[test]
    fn a_stereo_file_at_another_rate_comes_out_mono_at_the_voice_rate() {
        let decoded = decode(&wav(22_050, 2, 1.0), Some("wav")).expect("decodes");
        assert!(!decoded.truncated);
        let expected = SAMPLE_RATE as usize;
        let slack = 4 * RESAMPLER_CHUNK;
        assert!(
            decoded.pcm.len().abs_diff(expected) < slack,
            "one second became {} samples",
            decoded.pcm.len()
        );
        let peak = decoded.pcm.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(
            (0.4..0.6).contains(&peak),
            "a half-scale tone peaked at {peak}"
        );
    }

    #[test]
    fn a_long_file_is_cut_at_the_limit() {
        let decoded = decode(&wav(SAMPLE_RATE, 1, 8.0), Some("wav")).expect("decodes");
        assert!(decoded.truncated);
        assert_eq!(
            decoded.pcm.len(),
            (MAX_SOUND_SECS * SAMPLE_RATE as f32) as usize
        );
    }

    #[test]
    fn opposite_stereo_channels_cancel_when_mixed_to_mono() {
        let mut bytes = wav(SAMPLE_RATE, 2, 0.1);
        for frame in bytes[44..].as_chunks_mut::<4>().0 {
            let left = i16::from_le_bytes([frame[0], frame[1]]);
            frame[2..].copy_from_slice(&(-left).to_le_bytes());
        }
        let decoded = decode(&bytes, None).expect("stereo PCM decodes");
        assert!(!decoded.pcm.is_empty());
        assert!(decoded.pcm.iter().all(|sample| sample.abs() < 1e-6));
    }

    #[test]
    fn mp3_metadata_and_silent_sections_preserve_the_sound() {
        let bytes = include_bytes!("../tests/fixtures/tone-silence-stereo.mp3");
        assert!(bytes.starts_with(b"ID3"));
        let decoded = decode(bytes, Some("ogg")).expect("MP3 decodes despite its filename");
        assert!(!decoded.truncated);
        assert!(decoded.pcm.len().abs_diff(SAMPLE_RATE as usize) < 4 * RESAMPLER_CHUNK);
        let rate = SAMPLE_RATE as usize;
        let peak = |start, end| {
            decoded.pcm[rate * start / 10..rate * end / 10]
                .iter()
                .fold(0.0f32, |m, sample| m.max(sample.abs()))
        };
        assert!((0.3..0.45).contains(&peak(1, 2)));
        assert!(peak(4, 6) < 0.001);
        assert!((0.3..0.45).contains(&peak(8, 9)));
    }

    #[test]
    fn vorbis_stereo_at_another_rate_preserves_duration_and_mono_gain() {
        let bytes = include_bytes!("../tests/fixtures/tone-stereo.ogg");
        let decoded = decode(bytes, None).expect("Ogg Vorbis decodes");
        assert!(!decoded.truncated);
        assert!(decoded.pcm.len().abs_diff(SAMPLE_RATE as usize) < 4 * RESAMPLER_CHUNK);
        let peak = decoded.pcm.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(
            (0.3..0.45).contains(&peak),
            "stereo average peaked at {peak}"
        );
    }

    #[test]
    fn an_incomplete_pcm_packet_is_reported_as_damage() {
        let mut bytes = wav(SAMPLE_RATE, 2, 0.1);
        bytes.truncate(45);
        assert!(decode(&bytes, None).is_err());
    }

    #[test]
    fn pcm_sample_formats_keep_their_scale_and_silence() {
        let cases = [
            (1u16, 8u16, vec![0, 128, 255], [-1.0f32, 0.0, 127.0 / 128.0]),
            (1, 24, vec![0, 0, 128, 0, 0, 0, 0, 0, 64], [-1.0, 0.0, 0.5]),
            (
                3,
                32,
                [0.25f32, -0.5, 0.0]
                    .into_iter()
                    .flat_map(f32::to_le_bytes)
                    .collect(),
                [0.25, -0.5, 0.0],
            ),
        ];
        for (format, bits, data, expected) in cases {
            let mut bytes = wav(SAMPLE_RATE, 1, 0.0);
            bytes[4..8].copy_from_slice(&(36 + data.len() as u32).to_le_bytes());
            bytes[20..22].copy_from_slice(&format.to_le_bytes());
            bytes[28..32].copy_from_slice(&(SAMPLE_RATE * u32::from(bits / 8)).to_le_bytes());
            bytes[32..34].copy_from_slice(&(bits / 8).to_le_bytes());
            bytes[34..36].copy_from_slice(&bits.to_le_bytes());
            bytes[40..44].copy_from_slice(&(data.len() as u32).to_le_bytes());
            bytes.extend(data);
            let decoded = decode(&bytes, None).expect("PCM sample format decodes");
            assert_eq!(decoded.pcm.len(), expected.len());
            assert!(
                decoded
                    .pcm
                    .iter()
                    .zip(expected)
                    .all(|(a, b)| (a - b).abs() < 1e-6)
            );
        }
    }

    /// Discord's soundboard serves Ogg Opus with no extension; this is a tone
    /// made for the test, not one of theirs.
    const TONE_OPUS: &[u8] = include_bytes!("../tests/fixtures/tone-stereo.opus");

    fn opus_with_header_change(change: impl FnOnce(&mut [u8])) -> Vec<u8> {
        let mut bytes = TONE_OPUS.to_vec();
        let segments = usize::from(bytes[26]);
        let body_start = 27 + segments;
        let page_len = body_start
            + bytes[27..body_start]
                .iter()
                .map(|&size| usize::from(size))
                .sum::<usize>();
        change(&mut bytes[body_start..page_len]);
        bytes[22..26].fill(0);
        let mut crc = 0u32;
        for &byte in &bytes[..page_len] {
            crc ^= u32::from(byte) << 24;
            for _ in 0..8 {
                crc = if crc & 0x8000_0000 != 0 {
                    (crc << 1) ^ 0x04c1_1db7
                } else {
                    crc << 1
                };
            }
        }
        bytes[22..26].copy_from_slice(&crc.to_le_bytes());
        bytes
    }

    #[test]
    fn opus_header_gain_is_applied_once() {
        let original = decode(TONE_OPUS, None).expect("original Opus decodes");
        let quieter = opus_with_header_change(|head| {
            head[16..18].copy_from_slice(&(-6i16 * 256).to_le_bytes());
        });
        let decoded = decode(&quieter, None).expect("Opus with header gain decodes");
        assert_eq!(decoded.pcm.len(), original.pcm.len());
        let gain = 10f32.powf(-6.0 / 20.0);
        assert!(
            decoded
                .pcm
                .iter()
                .zip(&original.pcm)
                .all(|(a, b)| (a - b * gain).abs() < 1e-6)
        );
    }

    #[test]
    fn opus_pre_skip_can_span_multiple_packets() {
        let original = decode(TONE_OPUS, None).expect("original Opus decodes");
        let extra_skip = 1920u16;
        let bytes = opus_with_header_change(|head| {
            let original_skip = u16::from_le_bytes([head[10], head[11]]);
            head[10..12].copy_from_slice(&(original_skip + extra_skip).to_le_bytes());
        });
        let decoded = decode(&bytes, None).expect("Opus with longer pre-skip decodes");
        assert_eq!(decoded.pcm, original.pcm[usize::from(extra_skip)..]);
    }

    #[test]
    fn ogg_opus_decodes_with_its_pre_skip_removed() {
        let decoded = decode(TONE_OPUS, None).expect("Ogg Opus decodes");
        assert!(!decoded.truncated);
        let len = decoded.pcm.len();
        assert!(
            len.abs_diff(SAMPLE_RATE as usize) < 960,
            "one second of tone became {len} samples"
        );
        let peak = decoded.pcm.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(
            (0.35..0.65).contains(&peak),
            "a half-scale tone peaked at {peak}"
        );
    }

    #[test]
    fn the_bytes_name_the_format_not_the_file() {
        assert_eq!(sniff(TONE_OPUS), Some(("audio/ogg", "ogg")));
        assert_eq!(sniff(&wav(SAMPLE_RATE, 1, 0.1)), Some(("audio/wav", "wav")));
        assert_eq!(sniff(b"ID3\x04rest"), Some(("audio/mpeg", "mp3")));
        assert_eq!(sniff(b"\x89PNG\r\n"), None);
        let named_wrong = decode(TONE_OPUS, Some("mp3")).expect("the hint loses to the bytes");
        assert!(!named_wrong.pcm.is_empty());
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        assert!(decode(b"definitely not audio", Some("mp3")).is_err());
        assert!(decode(&[], None).is_err());
    }
}
