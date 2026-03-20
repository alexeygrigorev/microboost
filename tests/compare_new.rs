fn load_wav(path: &str) -> (Vec<f32>, u32) {
    let reader = hound::WavReader::open(path).expect(&format!("failed to open {}", path));
    let spec = reader.spec();
    let samples: Vec<f32> = if spec.sample_format == hound::SampleFormat::Int {
        let max = (1 << (spec.bits_per_sample - 1)) as f32;
        reader.into_samples::<i32>().map(|s| s.unwrap() as f32 / max).collect()
    } else {
        reader.into_samples::<f32>().map(|s| s.unwrap()).collect()
    };
    (samples, spec.sample_rate)
}

fn rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

#[test]
fn compare_pipeline_vs_original() {
    let path1 = "tests/fixtures/e2e_ring.wav";
    let path2 = "tests/fixtures/e2e_original.wav";
    if !std::path::Path::new(path1).exists() || !std::path::Path::new(path2).exists() {
        eprintln!("Skipping: {} or {} not found", path1, path2);
        return;
    }
    let (pipeline, pr) = load_wav(path1);
    let (original, or) = load_wav(path2);

    let p_rms = rms(&pipeline);
    let o_rms = rms(&original);
    let p_peak = pipeline.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
    let o_peak = original.iter().map(|s| s.abs()).fold(0.0f32, f32::max);

    eprintln!("=== Original ===");
    eprintln!("  {} samples, {}Hz, {:.2}s", original.len(), or, original.len() as f64 / or as f64);
    eprintln!("  RMS: {:.4} ({:.1} dBFS)", o_rms, 20.0 * o_rms.log10());
    eprintln!("  Peak: {:.4}", o_peak);

    eprintln!("\n=== Pipeline (ring buffer + CABLE) ===");
    eprintln!("  {} samples, {}Hz, {:.2}s", pipeline.len(), pr, pipeline.len() as f64 / pr as f64);
    eprintln!("  RMS: {:.4} ({:.1} dBFS)", p_rms, 20.0 * p_rms.log10());
    eprintln!("  Peak: {:.4}", p_peak);
    eprintln!("  Boost ratio (RMS): {:.2}x", p_rms / o_rms);

    // Check for clipping (samples at or near ±1.0)
    let clip_count = pipeline.iter().filter(|s| s.abs() > 0.99).count();
    eprintln!("  Clipped samples (>0.99): {} ({:.2}%)", clip_count, clip_count as f64 / pipeline.len() as f64 * 100.0);

    // Check for zero insertions (underruns)
    let mut zero_runs = 0;
    let mut in_zero = false;
    for &s in &pipeline {
        if s.abs() < 1e-7 {
            if !in_zero {
                zero_runs += 1;
                in_zero = true;
            }
        } else {
            in_zero = false;
        }
    }
    eprintln!("  Zero-runs (potential underruns): {}", zero_runs);

    // Large jumps (clicks/glitches)
    let mut large_jumps = 0;
    for i in 1..pipeline.len() {
        if (pipeline[i] - pipeline[i-1]).abs() > 0.3 {
            large_jumps += 1;
            if large_jumps <= 10 {
                eprintln!("  Jump at sample {}: {:.4} -> {:.4} (delta={:.4})",
                    i, pipeline[i-1], pipeline[i], pipeline[i] - pipeline[i-1]);
            }
        }
    }
    eprintln!("  Large jumps (>0.3): {}", large_jumps);

    // Spectral analysis: check energy in high frequencies (noise/distortion)
    // Use windowed RMS in 1ms windows
    let window = 48; // 1ms at 48kHz
    let mut pipeline_windows: Vec<f32> = pipeline.chunks(window).map(|w| rms(w)).collect();
    let mut original_windows: Vec<f32> = original.chunks(window).map(|w| rms(w)).collect();
    pipeline_windows.sort_by(|a, b| a.partial_cmp(b).unwrap());
    original_windows.sort_by(|a, b| a.partial_cmp(b).unwrap());

    // Noise floor: average of quietest 10%
    let n_p = pipeline_windows.len() / 10;
    let n_o = original_windows.len() / 10;
    let p_floor = if n_p > 0 { pipeline_windows[..n_p].iter().sum::<f32>() / n_p as f32 } else { 0.0 };
    let o_floor = if n_o > 0 { original_windows[..n_o].iter().sum::<f32>() / n_o as f32 } else { 0.0 };

    eprintln!("\n=== Noise floor (quietest 10%) ===");
    eprintln!("  Original: {:.6} ({:.1} dBFS)", o_floor, if o_floor > 0.0 { 20.0 * o_floor.log10() } else { -100.0 });
    eprintln!("  Pipeline: {:.6} ({:.1} dBFS)", p_floor, if p_floor > 0.0 { 20.0 * p_floor.log10() } else { -100.0 });

    // Sample-by-sample derivative analysis (smoothness)
    let p_deriv_rms: f32 = {
        let sum: f32 = (1..pipeline.len()).map(|i| {
            let d = pipeline[i] - pipeline[i-1];
            d * d
        }).sum();
        (sum / pipeline.len() as f32).sqrt()
    };
    let o_deriv_rms: f32 = {
        let sum: f32 = (1..original.len()).map(|i| {
            let d = original[i] - original[i-1];
            d * d
        }).sum();
        (sum / original.len() as f32).sqrt()
    };
    eprintln!("\n=== Signal smoothness (derivative RMS) ===");
    eprintln!("  Original: {:.6}", o_deriv_rms);
    eprintln!("  Pipeline: {:.6} ({:.1}x rougher)", p_deriv_rms, p_deriv_rms / o_deriv_rms);
}
