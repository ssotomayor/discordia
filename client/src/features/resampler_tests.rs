use super::{AudioResampler, RESAMPLER_CHUNK};
use rubato::Resampler;
use rubato_legacy::{FftFixedIn, Resampler as LegacyResampler};

const RATES: &[(u32, u32)] = &[
    (8_000, 48_000),
    (11_025, 48_000),
    (12_000, 48_000),
    (16_000, 48_000),
    (22_050, 48_000),
    (24_000, 48_000),
    (32_000, 48_000),
    (44_100, 48_000),
    (88_200, 48_000),
    (96_000, 48_000),
    (192_000, 48_000),
    (48_000, 8_000),
    (48_000, 16_000),
    (48_000, 22_050),
    (48_000, 32_000),
    (48_000, 44_100),
    (48_000, 96_000),
    (48_000, 192_000),
];

fn legacy(input: &[f32], from: u32, to: u32) -> (Vec<f32>, usize) {
    let mut inner =
        FftFixedIn::<f32>::new(from as usize, to as usize, RESAMPLER_CHUNK, 2, 1).unwrap();
    let delay = inner.output_delay();
    let mut scratch = vec![0.0; inner.output_frames_max()];
    let mut out = Vec::new();
    for chunk in input.as_chunks::<RESAMPLER_CHUNK>().0 {
        let (_, produced) = inner
            .process_into_buffer(&[chunk], &mut [&mut scratch[..]], None)
            .unwrap();
        out.extend_from_slice(&scratch[..produced]);
    }
    (out, delay)
}

fn stream(input: &[f32], from: u32, to: u32, sizes: &[usize]) -> Vec<f32> {
    let mut resampler = AudioResampler::new(from, to).unwrap();
    let mut out = Vec::new();
    let mut block = Vec::new();
    let mut position = 0;
    for size in sizes.iter().cycle() {
        let end = (position + size).min(input.len());
        resampler.process_into(&input[position..end], &mut block);
        out.extend_from_slice(&block);
        position = end;
        if position == input.len() {
            break;
        }
    }
    out
}

#[test]
fn rate_matrix_preserves_legacy_samples_counts_and_delay() {
    for &(from, to) in RATES {
        for impulse in [false, true] {
            let mut input: Vec<f32> = (0..from as usize)
                .map(|n| {
                    if impulse {
                        0.0
                    } else {
                        0.4 * (n as f32 * 1000.0 * std::f32::consts::TAU / from as f32).sin()
                    }
                })
                .collect();
            if impulse {
                input[13] = 1.0;
            }
            input.resize(input.len() + 4 * RESAMPLER_CHUNK, 0.0);
            let (reference, delay) = legacy(&input, from, to);
            let actual = stream(&input, from, to, &[480]);
            assert_eq!(actual.len(), reference.len(), "frame count {from}->{to}");
            assert_eq!(
                AudioResampler::new(from, to).unwrap().inner.output_delay(),
                delay,
                "delay {from}->{to}"
            );
            let error = actual
                .iter()
                .zip(&reference)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0_f32, f32::max);
            assert!(actual.iter().all(|s| s.is_finite()));
            assert!(
                error < 0.00002,
                "sample error {error}: {from}->{to}, impulse={impulse}"
            );
        }
    }
}

#[test]
fn partial_chunks_preserve_output_independently_of_input_partition() {
    let input: Vec<f32> = (0..17_333)
        .map(|n| (n as f32 * 0.017).sin() * 0.3)
        .collect();
    for &(from, to) in RATES {
        let reference = stream(&input, from, to, &[input.len()]);
        for sizes in [
            &[1][..],
            &[127],
            &[480],
            &[512],
            &[1024],
            &[1, 127, 480, 1024, 31],
        ] {
            assert_eq!(
                stream(&input, from, to, sizes),
                reference,
                "partition {sizes:?}: {from}->{to}"
            );
        }
    }
}

#[test]
fn equal_rates_bypass_and_warmed_buffers_keep_their_capacity() {
    assert!(AudioResampler::new(48_000, 48_000).is_none());
    for &(from, to) in RATES {
        let mut resampler = AudioResampler::new(from, to).unwrap();
        let input = vec![0.1; from as usize / 100];
        let mut out = Vec::new();
        for _ in 0..20 {
            resampler.process_into(&input, &mut out);
        }
        let capacities = (
            resampler.input_accum.capacity(),
            resampler.chunk_in.capacity(),
            resampler.scratch_out.capacity(),
            out.capacity(),
        );
        for _ in 0..200 {
            resampler.process_into(&input, &mut out);
            assert_eq!(
                (
                    resampler.input_accum.capacity(),
                    resampler.chunk_in.capacity(),
                    resampler.scratch_out.capacity(),
                    out.capacity()
                ),
                capacities,
                "buffer growth {from}->{to}"
            );
        }
    }
}

#[test]
fn short_clip_tail_padding_matches_legacy() {
    for &(from, to) in RATES {
        for length in [1, 127, 480, 511, 512, 513, 1023] {
            let mut input = vec![0.0; length + 2 * RESAMPLER_CHUNK];
            input[length - 1] = 0.7;
            let (reference, _) = legacy(&input, from, to);
            let actual = stream(&input, from, to, &[input.len()]);
            assert_eq!(actual.len(), reference.len());
            assert!(
                actual
                    .iter()
                    .zip(&reference)
                    .all(|(a, b)| (a - b).abs() < 0.00002)
            );
        }
    }
}

#[test]
fn invalid_rates_and_empty_calls_preserve_the_contract() {
    assert!(AudioResampler::new(0, 48_000).is_none());
    assert!(AudioResampler::new(48_000, 0).is_none());
    let mut resampler = AudioResampler::new(44_100, 48_000).unwrap();
    let mut out = vec![1.0];
    resampler.process_into(&[0.2; 127], &mut out);
    assert!(out.is_empty());
    resampler.process_into(&[], &mut out);
    assert!(out.is_empty());
    assert_eq!(resampler.input_accum.len(), 127);
    resampler.process_into(&[0.2; 385], &mut out);
    assert_eq!(out, stream(&[0.2; 512], 44_100, 48_000, &[512]));
}
