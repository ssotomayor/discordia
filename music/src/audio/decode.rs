use super::{AudioError, CHANNELS, FRAME_SAMPLES, Frame, RATE};
use rubato::{
    Fft, FixedSync, Resampler, WindowFunction, audioadapter_buffers::direct::InterleavedSlice,
};
use std::{collections::VecDeque, fs::File, path::Path};
use symphonia::core::{
    codecs::audio::{AudioDecoderOptions, CODEC_ID_NULL_AUDIO},
    common::Limit,
    formats::{FormatOptions, probe::Hint},
    io::MediaSourceStream,
    meta::MetadataOptions,
};
use tokio::sync::mpsc::Sender;

const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_PACKET_BYTES: usize = 1024 * 1024;
const MAX_PACKET_FRAMES: usize = 32768;
const RESAMPLE_CHUNK: usize = 1024;

pub(super) fn file(
    path: &Path,
    sender: &Sender<Result<Frame, AudioError>>,
) -> Result<(), AudioError> {
    let file = File::open(path).map_err(|_| AudioError::Input)?;
    let metadata = file.metadata().map_err(|_| AudioError::Input)?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(AudioError::Input);
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(AudioError::ResourceLimit);
    }
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(extension);
    }
    let metadata = MetadataOptions::default()
        .limit_tag_bytes(Limit::Maximum(16 * 1024))
        .limit_visual_bytes(Limit::Maximum(0));
    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            MediaSourceStream::new(Box::new(file), Default::default()),
            FormatOptions::default(),
            metadata,
        )
        .map_err(|_| AudioError::Unsupported)?;
    let (track_id, mut decoder, rate) = {
        let (track, params) = format
            .tracks()
            .iter()
            .filter_map(|t| t.codec_params.as_ref()?.audio().map(|p| (t, p)))
            .find(|(_, p)| p.codec != CODEC_ID_NULL_AUDIO)
            .ok_or(AudioError::Unsupported)?;
        let rate = params.sample_rate.ok_or(AudioError::Unsupported)?;
        if !(8000..=192000).contains(&rate) {
            return Err(AudioError::Unsupported);
        }
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(params, &AudioDecoderOptions::default())
            .map_err(|_| AudioError::Unsupported)?;
        (track.id, decoder, rate)
    };
    let mut output = Output::new(rate, sender)?;
    let mut samples = Vec::<f32>::new();
    loop {
        if sender.is_closed() {
            return Err(AudioError::Cancelled);
        }
        let Some(packet) = format.next_packet().map_err(|_| AudioError::Corrupt)? else {
            break;
        };
        if packet.track_id != track_id {
            continue;
        }
        if packet.data.len() > MAX_PACKET_BYTES {
            return Err(AudioError::ResourceLimit);
        }
        let buffer = decoder.decode(&packet).map_err(|_| AudioError::Corrupt)?;
        let channels = buffer.spec().channels().count();
        if !(1..=2).contains(&channels) || buffer.spec().rate() != rate {
            return Err(AudioError::Unsupported);
        }
        if buffer.frames() > MAX_PACKET_FRAMES {
            return Err(AudioError::ResourceLimit);
        }
        samples.clear();
        buffer.copy_to_vec_interleaved(&mut samples);
        for frame in samples.chunks_exact(channels) {
            let stereo = [frame[0], frame[channels - 1]];
            if stereo.iter().any(|v| !v.is_finite()) {
                return Err(AudioError::Corrupt);
            }
            output.push(stereo)?;
        }
    }
    output.finish()
}

struct Output<'a> {
    sender: &'a Sender<Result<Frame, AudioError>>,
    resampler: Option<Fft<f32>>,
    input: Vec<f32>,
    scratch: Vec<f32>,
    pending: VecDeque<f32>,
    skip: usize,
    input_frames: u64,
    output_frames: u64,
    rate: u32,
}

impl<'a> Output<'a> {
    fn new(rate: u32, sender: &'a Sender<Result<Frame, AudioError>>) -> Result<Self, AudioError> {
        let resampler = if rate == RATE {
            None
        } else {
            Some(
                Fft::<f32>::new_custom(
                    rate as usize,
                    RATE as usize,
                    RESAMPLE_CHUNK,
                    2,
                    CHANNELS,
                    WindowFunction::BlackmanHarris2,
                    FixedSync::Input,
                )
                .map_err(|_| AudioError::Unsupported)?,
            )
        };
        let skip = resampler
            .as_ref()
            .map_or(0, |r| r.output_delay() * CHANNELS);
        let scratch = vec![
            0.0;
            resampler
                .as_ref()
                .map_or(0, |r| r.output_frames_max() * CHANNELS)
        ];
        Ok(Self {
            sender,
            resampler,
            input: Vec::new(),
            scratch,
            pending: VecDeque::new(),
            skip,
            input_frames: 0,
            output_frames: 0,
            rate,
        })
    }

    fn push(&mut self, stereo: [f32; 2]) -> Result<(), AudioError> {
        self.input_frames += 1;
        if self.resampler.is_some() {
            self.input.extend_from_slice(&stereo);
            if self.input.len()
                >= self
                    .resampler
                    .as_ref()
                    .ok_or(AudioError::Worker)?
                    .input_frames_next()
                    * CHANNELS
            {
                self.resample()?;
            }
        } else {
            self.pending.extend(stereo);
        }
        self.emit(false)
    }

    fn resample(&mut self) -> Result<(), AudioError> {
        let resampler = self.resampler.as_mut().ok_or(AudioError::Worker)?;
        let needed = resampler.input_frames_next();
        self.input.resize(needed * CHANNELS, 0.0);
        let input =
            InterleavedSlice::new(&self.input, CHANNELS, needed).map_err(|_| AudioError::Worker)?;
        let mut output =
            InterleavedSlice::new_mut(&mut self.scratch, CHANNELS, resampler.output_frames_max())
                .map_err(|_| AudioError::Worker)?;
        let (_, produced) = resampler
            .process_into_buffer(&input, &mut output, None)
            .map_err(|_| AudioError::Corrupt)?;
        self.input.clear();
        let skip = self.skip.min(produced * CHANNELS);
        self.skip -= skip;
        self.pending
            .extend(self.scratch[skip..produced * CHANNELS].iter().copied());
        Ok(())
    }

    fn emit(&mut self, finishing: bool) -> Result<(), AudioError> {
        let target = self.input_frames * u64::from(RATE) / u64::from(self.rate);
        loop {
            let remaining = (target - self.output_frames) as usize;
            let count = if finishing {
                remaining.min(FRAME_SAMPLES)
            } else {
                FRAME_SAMPLES
            };
            if count == 0 || remaining < count || self.pending.len() < count * CHANNELS {
                break;
            }
            let mut frame = Frame::silence();
            frame.valid_frames = count;
            for sample in frame.samples.iter_mut().take(count * CHANNELS) {
                let value = self.pending.pop_front().ok_or(AudioError::Worker)?;
                *sample = (value.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16;
            }
            self.sender
                .blocking_send(Ok(frame))
                .map_err(|_| AudioError::Cancelled)?;
            self.output_frames += count as u64;
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), AudioError> {
        if self.input_frames == 0 {
            return Err(AudioError::Corrupt);
        }
        let target = self.input_frames * u64::from(RATE) / u64::from(self.rate);
        while self.resampler.is_some()
            && (self.skip > 0
                || self.pending.len() < (target - self.output_frames) as usize * CHANNELS)
        {
            self.resample()?;
        }
        self.emit(true)?;
        if self.output_frames != target {
            return Err(AudioError::Corrupt);
        }
        Ok(())
    }
}
