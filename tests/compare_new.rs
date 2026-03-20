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

#[ignore] // requires manual recording
#[test]
fn compare_boosted_vs_direct() {
    let (boosted, br) = load_wav("tests/.tmp/rec_boosted.wav");
    let (direct, dr) = load_wav("tests/.tmp/rec_direct.wav");

    let b_rms = rms(&boosted);
    let d_rms = rms(&direct);
    let b_peak = boosted.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
    let d_peak = direct.iter().map(|s| s.abs()).fold(0.0f32, f32::max);

    eprintln!("=== Direct mic ===");
    eprintln!("  {} samples, {}Hz, {:.2}s", direct.len(), dr, direct.len() as f64 / dr as f64);
    eprintln!("  RMS: {:.4} ({:.1} dBFS)", d_rms, 20.0 * d_rms.log10());
    eprintln!("  Peak: {:.4}", d_peak);

    eprintln!("\n=== Boosted (through CABLE) ===");
    eprintln!("  {} samples, {}Hz, {:.2}s", boosted.len(), br, boosted.len() as f64 / br as f64);
    eprintln!("  RMS: {:.4} ({:.1} dBFS)", b_rms, 20.0 * b_rms.log10());
    eprintln!("  Peak: {:.4}", b_peak);
    eprintln!("  Boost ratio (RMS): {:.2}x", b_rms / d_rms);

    // Check for clipping (samples at or near ±1.0)
    let clip_count = boosted.iter().filter(|s| s.abs() > 0.99).count();
    eprintln!("  Clipped samples (>0.99): {} ({:.2}%)", clip_count, clip_count as f64 / boosted.len() as f64 * 100.0);

    // Check for zero insertions (underruns)
    let mut zero_runs = 0;
    let mut in_zero = false;
    for &s in &boosted {
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
    for i in 1..boosted.len() {
        if (boosted[i] - boosted[i-1]).abs() > 0.3 {
            large_jumps += 1;
            if large_jumps <= 10 {
                eprintln!("  Jump at sample {}: {:.4} -> {:.4} (delta={:.4})",
                    i, boosted[i-1], boosted[i], boosted[i] - boosted[i-1]);
            }
        }
    }
    eprintln!("  Large jumps (>0.3): {}", large_jumps);

    // Spectral analysis: check energy in high frequencies (noise/distortion)
    // Use windowed RMS in 1ms windows
    let window = 48; // 1ms at 48kHz
    let mut boosted_windows: Vec<f32> = boosted.chunks(window).map(|w| rms(w)).collect();
    let mut direct_windows: Vec<f32> = direct.chunks(window).map(|w| rms(w)).collect();
    boosted_windows.sort_by(|a, b| a.partial_cmp(b).unwrap());
    direct_windows.sort_by(|a, b| a.partial_cmp(b).unwrap());

    // Noise floor: average of quietest 10%
    let n_b = boosted_windows.len() / 10;
    let n_d = direct_windows.len() / 10;
    let b_floor = if n_b > 0 { boosted_windows[..n_b].iter().sum::<f32>() / n_b as f32 } else { 0.0 };
    let d_floor = if n_d > 0 { direct_windows[..n_d].iter().sum::<f32>() / n_d as f32 } else { 0.0 };

    eprintln!("\n=== Noise floor (quietest 10%) ===");
    eprintln!("  Direct: {:.6} ({:.1} dBFS)", d_floor, if d_floor > 0.0 { 20.0 * d_floor.log10() } else { -100.0 });
    eprintln!("  Boosted: {:.6} ({:.1} dBFS)", b_floor, if b_floor > 0.0 { 20.0 * b_floor.log10() } else { -100.0 });

    // Sample-by-sample derivative analysis (smoothness)
    let b_deriv_rms: f32 = {
        let sum: f32 = (1..boosted.len()).map(|i| {
            let d = boosted[i] - boosted[i-1];
            d * d
        }).sum();
        (sum / boosted.len() as f32).sqrt()
    };
    let d_deriv_rms: f32 = {
        let sum: f32 = (1..direct.len()).map(|i| {
            let d = direct[i] - direct[i-1];
            d * d
        }).sum();
        (sum / direct.len() as f32).sqrt()
    };
    eprintln!("\n=== Signal smoothness (derivative RMS) ===");
    eprintln!("  Direct: {:.6}", d_deriv_rms);
    eprintln!("  Boosted: {:.6} ({:.1}x rougher)", b_deriv_rms, b_deriv_rms / d_deriv_rms);
}
