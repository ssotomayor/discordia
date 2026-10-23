// The included production file brings `Duration` and `Instant` with it.
include!("frame_pacer_under_test.rs");

fn measure(target: u32, source_hz: f64, jitter_us: u64, mut seed: u64) -> (usize, usize, f64) {
    let start = Instant::now();
    let mut pacer = FramePacer::new(target);
    let mut callbacks = 0;
    let mut accepted = 0;
    let mut previous = None;
    let mut largest_gap: f64 = 0.0;
    for index in 0.. {
        let nominal_us = (index as f64 * 1_000_000.0 / source_hz).round() as u64;
        if nominal_us >= 120_000_000 {
            break;
        }
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let delay = if jitter_us == 0 {
            0
        } else {
            (seed >> 32) % (jitter_us + 1)
        };
        let now = start + Duration::from_micros(nominal_us + delay);
        callbacks += 1;
        if pacer.accept(now) {
            accepted += 1;
            if let Some(last) = previous {
                largest_gap = largest_gap.max(now.duration_since(last).as_secs_f64() * 1000.0);
            }
            previous = Some(now);
        }
    }
    (callbacks, accepted, largest_gap)
}

fn main() {
    assert_eq!(measure(60, 60.0, 0, 42).1, 7200);
    assert!((3600..=3601).contains(&measure(30, 60.0, 0, 42).1));
    let start = Instant::now();
    let mut stalled = FramePacer::new(60);
    assert!(stalled.accept(start));
    assert!(stalled.accept(start + Duration::from_secs(10)));
    assert!(!stalled.accept(start + Duration::from_secs(10) + Duration::from_millis(2)));
    println!(
        "target_fps,source_hz,callback_delay_max_ms,seed,source_callbacks,accepted_frames,accepted_fps,largest_output_gap_ms"
    );
    for target in [15, 30, 60] {
        for source_hz in [59.94, 60.0, 120.0, 144.0, 165.0] {
            for jitter_us in [0, 400, 2000, 4000] {
                for seed in [42, 7, 123456] {
                    let (callbacks, accepted, gap) = measure(target, source_hz, jitter_us, seed);
                    assert!(accepted <= callbacks);
                    assert!(accepted <= target as usize * 120 + 1);
                    if target == 60 && source_hz <= 60.0 {
                        assert_eq!(accepted, callbacks, "unnecessary capture drops");
                    }
                    println!(
                        "{target},{source_hz},{:.1},{seed},{callbacks},{accepted},{:.3},{gap:.3}",
                        jitter_us as f64 / 1000.0,
                        accepted as f64 / 120.0
                    );
                }
            }
        }
    }
}
