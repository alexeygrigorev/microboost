/// Test the actual microboost pipeline: SpscRing + NoiseGate + gain processing.
/// Feeds a real voice WAV through the same code path as the live audio callbacks.

use microboost::{noise_gate, SpscRing, RING_SIZE};

fn load_wav(path: &str) -> (Vec<f32>, u32) {
    let reader = hound::WavReader::open(path).expect("failed to open WAV");
    let spec = reader.spec();
    let samples: Vec<f32> = if spec.sample_format == hound::SampleFormat::Int {
        let max = (1 << (spec.bits_per_sample - 1)) as f32;
        reader
            .into_samples::<i32>()
            .map(|s| s.unwrap() as f32 / max)
            .collect()
    } else {
        reader.into_samples::<f32>().map(|s| s.unwrap()).collect()
    };
    (samples, spec.sample_rate)
}

/// Feed input through ring in chunks (simulating real audio callbacks),
/// collect output. This is how the real pipeline works.
fn run_pipeline(
    input: &[f32],
    gain: f32,
    gate: &mut noise_gate::NoiseGate,
    rate_ratio: f64,
) -> Vec<f32> {
    let ring = SpscRing::new(RING_SIZE);
    let mut output = Vec::new();
    let chunk_size = 480; // 10ms at 48kHz, typical callback size
    let mut frac: f64 = 0.0;

    for chunk in input.chunks(chunk_size) {
        // === Input callback: gain + gate + push ===
        for &sample in chunk {
            let boosted = (sample * gain).clamp(-1.0, 1.0);
            let gated = gate.process(boosted);
            ring.push(gated);
        }

        // === Output callback: read with interpolation ===
        while ring.available() > 1 {
            let s0 = ring.peek(0);
            let s1 = ring.peek(1);
            let t = frac as f32;
            let interpolated = s0 + (s1 - s0) * t;
            output.push(interpolated);

            frac += rate_ratio;
            while frac >= 1.0 {
                frac -= 1.0;
                if ring.available() > 0 {
                    ring.advance(1);
                }
            }
        }
    }

    // Drain remaining
    while ring.available() > 1 {
        let s0 = ring.peek(0);
        let s1 = ring.peek(1);
        let t = frac as f32;
        output.push(s0 + (s1 - s0) * t);
        frac += rate_ratio;
        while frac >= 1.0 {
            frac -= 1.0;
            if ring.available() > 0 {
                ring.advance(1);
            }
        }
    }

    output
}

#[test]
fn real_voice_1x_passthrough() {
    let (input, _) = load_wav("tests/fixtures/test_voice.wav");
    assert!(input.len() > 48000, "recording too short");

    let mut gate = noise_gate::NoiseGate::new(); // disabled
    let output = run_pipeline(&input, 1.0, &mut gate, 1.0);

    // At 1x gain, rate_ratio 1.0, gate disabled: output must match input.
    // The ring may leave 1 sample behind, so allow off-by-one on length.
    let compare_len = input.len().min(output.len());
    assert!(
        (input.len() as i64 - output.len() as i64).unsigned_abs() <= 1,
        "length mismatch: input={}, output={}",
        input.len(),
        output.len()
    );

    let mut max_diff: f32 = 0.0;
    for i in 0..compare_len {
        let diff = (input[i] - output[i]).abs();
        max_diff = max_diff.max(diff);
        assert_eq!(
            input[i], output[i],
            "Sample {} differs: input={}, output={}",
            i, input[i], output[i]
        );
    }
    eprintln!(
        "Passed: {} samples, max_diff={}",
        compare_len, max_diff
    );
}

#[test]
fn real_voice_2x_boost() {
    let (input, _) = load_wav("tests/fixtures/test_voice.wav");
    let mut gate = noise_gate::NoiseGate::new();
    let output = run_pipeline(&input, 2.0, &mut gate, 1.0);

    let compare_len = input.len().min(output.len());
    assert!(
        (input.len() as i64 - output.len() as i64).unsigned_abs() <= 1,
        "length mismatch"
    );

    for i in 0..compare_len {
        let expected = (input[i] * 2.0).clamp(-1.0, 1.0);
        let diff = (expected - output[i]).abs();
        assert!(
            diff < 1e-6,
            "Sample {} differs: expected={}, output={}, diff={}",
            i, expected, output[i], diff
        );
    }
}

#[test]
fn real_voice_050x_attenuate() {
    let (input, _) = load_wav("tests/fixtures/test_voice.wav");
    let mut gate = noise_gate::NoiseGate::new();
    let output = run_pipeline(&input, 0.5, &mut gate, 1.0);

    let compare_len = input.len().min(output.len());
    for i in 0..compare_len {
        let expected = (input[i] * 0.5).clamp(-1.0, 1.0);
        let diff = (expected - output[i]).abs();
        assert!(
            diff < 1e-6,
            "Sample {} differs: expected={}, output={}",
            i, expected, output[i]
        );
    }
}

#[test]
fn real_voice_with_noise_gate() {
    let (input, _) = load_wav("tests/fixtures/test_voice.wav");

    let mut gate = noise_gate::NoiseGate::new();
    let silence = vec![0.001f32; 48000];
    gate.finish_calibration(&silence).unwrap();
    assert!(gate.enabled);

    let output = run_pipeline(&input, 1.0, &mut gate, 1.0);

    let compare_len = input.len().min(output.len());
    let mut gated_count = 0;
    for i in 0..compare_len {
        assert!(
            output[i].abs() <= input[i].abs() + 1e-6,
            "Sample {} louder than input: in={}, out={}",
            i, input[i], output[i]
        );
        if (input[i] - output[i]).abs() > 1e-6 {
            gated_count += 1;
        }
    }
    assert!(gated_count > 0, "noise gate didn't gate anything");
    eprintln!(
        "Noise gate: {}/{} samples modified ({:.1}%)",
        gated_count,
        compare_len,
        gated_count as f64 / compare_len as f64 * 100.0
    );
}

#[test]
fn pipeline_mismatched_sample_rate() {
    let (input, _) = load_wav("tests/fixtures/test_voice.wav");
    let mut gate = noise_gate::NoiseGate::new();

    // Simulate 48kHz input, 44.1kHz output
    let rate_ratio = 48000.0 / 44100.0;
    let output = run_pipeline(&input, 1.0, &mut gate, rate_ratio);

    let expected_len = (input.len() as f64 / rate_ratio) as usize;
    let len_diff = (output.len() as i64 - expected_len as i64).unsigned_abs();
    assert!(
        len_diff < 100,
        "unexpected output length: got {}, expected ~{}",
        output.len(),
        expected_len
    );

    for (i, &s) in output.iter().enumerate() {
        assert!(s.is_finite(), "Sample {} is NaN/Inf: {}", i, s);
        assert!(s.abs() <= 1.0, "Sample {} clipping: {}", i, s);
    }
}
