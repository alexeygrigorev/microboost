/// Automated quality check: compare dual_mic.wav vs dual_cable.wav
/// Check for distortion, noise, frequency response issues.

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
    if samples.is_empty() { return 0.0; }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

fn rms_db(r: f32) -> f32 {
    if r > 0.0 { 20.0 * r.log10() } else { -100.0 }
}

/// Compute energy in frequency bands using DFT on a window
fn band_energy(samples: &[f32], sample_rate: u32, lo_hz: f32, hi_hz: f32) -> f32 {
    let n = samples.len().min(4096);
    let mut energy = 0.0f64;
    let bin_lo = (lo_hz * n as f32 / sample_rate as f32) as usize;
    let bin_hi = (hi_hz * n as f32 / sample_rate as f32) as usize;
    for bin in bin_lo..=bin_hi.min(n / 2) {
        let mut real = 0.0f64;
        let mut imag = 0.0f64;
        for (i, &s) in samples[..n].iter().enumerate() {
            let angle = 2.0 * std::f64::consts::PI * bin as f64 * i as f64 / n as f64;
            real += s as f64 * angle.cos();
            imag += s as f64 * (-angle.sin());
        }
        energy += real * real + imag * imag;
    }
    energy as f32
}

#[test]
fn quality_comparison() {
    let path1 = "tests/fixtures/e2e_original.wav";
    let path2 = "tests/fixtures/e2e_ring.wav";
    if !std::path::Path::new(path1).exists() || !std::path::Path::new(path2).exists() {
        eprintln!("Skipping: {} or {} not found", path1, path2);
        return;
    }
    let (mic, mic_rate) = load_wav(path1);
    let (cable, cable_rate) = load_wav(path2);

    eprintln!("=== Basic stats ===");
    let m_rms = rms(&mic);
    let c_rms = rms(&cable);
    let m_peak = mic.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
    let c_peak = cable.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
    eprintln!("Original (mic): {} samples, RMS={:.4} ({:.1}dB), peak={:.4}", mic.len(), m_rms, rms_db(m_rms), m_peak);
    eprintln!("Ring pipeline (cable): {} samples, RMS={:.4} ({:.1}dB), peak={:.4}", cable.len(), c_rms, rms_db(c_rms), c_peak);
    eprintln!("Ratio: {:.2}x", c_rms / m_rms);

    // === Clipping check ===
    let clip_count = cable.iter().filter(|s| s.abs() > 0.99).count();
    let clip_pct = clip_count as f64 / cable.len() as f64 * 100.0;
    eprintln!("\n=== Clipping ===");
    eprintln!("Cable clipped samples: {} ({:.3}%)", clip_count, clip_pct);

    // === Noise floor (quietest 10% of 10ms windows) ===
    let window = (mic_rate / 100) as usize; // 10ms
    let mut mic_w: Vec<f32> = mic.chunks(window).map(|w| rms(w)).collect();
    let mut cable_w: Vec<f32> = cable.chunks(window).map(|w| rms(w)).collect();
    mic_w.sort_by(|a, b| a.partial_cmp(b).unwrap());
    cable_w.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n10_m = mic_w.len() / 10;
    let n10_c = cable_w.len() / 10;
    let m_floor = if n10_m > 0 { mic_w[..n10_m].iter().sum::<f32>() / n10_m as f32 } else { 0.0 };
    let c_floor = if n10_c > 0 { cable_w[..n10_c].iter().sum::<f32>() / n10_c as f32 } else { 0.0 };
    eprintln!("\n=== Noise floor ===");
    eprintln!("Mic:   {:.6} ({:.1}dB)", m_floor, rms_db(m_floor));
    eprintln!("Cable: {:.6} ({:.1}dB)", c_floor, rms_db(c_floor));

    // === Smoothness (derivative RMS — higher = rougher/more distorted) ===
    let deriv_rms = |d: &[f32]| -> f32 {
        if d.len() < 2 { return 0.0; }
        let sum: f32 = (1..d.len()).map(|i| { let x = d[i] - d[i-1]; x * x }).sum();
        (sum / d.len() as f32).sqrt()
    };
    let m_drms = deriv_rms(&mic);
    let c_drms = deriv_rms(&cable);
    eprintln!("\n=== Signal smoothness (derivative RMS) ===");
    eprintln!("Mic:   {:.6}", m_drms);
    eprintln!("Cable: {:.6}", c_drms);
    // Normalize by signal level for fair comparison
    let m_drms_norm = m_drms / m_rms;
    let c_drms_norm = c_drms / c_rms;
    eprintln!("Normalized (÷ RMS): mic={:.4}, cable={:.4}, ratio={:.2}x", m_drms_norm, c_drms_norm, c_drms_norm / m_drms_norm);

    // === Large jumps (clicks/glitches) ===
    let jump_thresh = 0.2;
    let m_jumps = (1..mic.len()).filter(|&i| (mic[i] - mic[i-1]).abs() > jump_thresh).count();
    let c_jumps = (1..cable.len()).filter(|&i| (cable[i] - cable[i-1]).abs() > jump_thresh).count();
    eprintln!("\n=== Large jumps (>{}) ===", jump_thresh);
    eprintln!("Mic:   {}", m_jumps);
    eprintln!("Cable: {}", c_jumps);

    // === Frequency balance (compare low/mid/high energy ratios) ===
    // Find a voiced segment (loudest 1-second window)
    let one_sec = mic_rate as usize;
    let find_loud = |d: &[f32]| -> usize {
        if d.len() < one_sec { return 0; }
        (0..d.len() - one_sec)
            .step_by(one_sec / 4)
            .max_by(|&a, &b| {
                let ra = rms(&d[a..a + one_sec]);
                let rb = rms(&d[b..b + one_sec]);
                ra.partial_cmp(&rb).unwrap()
            })
            .unwrap_or(0)
    };
    let m_start = find_loud(&mic);
    let c_start = find_loud(&cable);

    let m_seg = &mic[m_start..m_start + one_sec.min(mic.len() - m_start)];
    let c_seg = &cable[c_start..c_start + one_sec.min(cable.len() - c_start)];

    let bands = [(100.0, 500.0, "Low"), (500.0, 2000.0, "Mid"), (2000.0, 8000.0, "High")];
    eprintln!("\n=== Frequency balance (loudest 1s segment) ===");
    let mut m_total = 0.0f32;
    let mut c_total = 0.0f32;
    let mut m_bands = vec![];
    let mut c_bands = vec![];
    for &(lo, hi, _) in &bands {
        let me = band_energy(m_seg, mic_rate, lo, hi);
        let ce = band_energy(c_seg, cable_rate, lo, hi);
        m_total += me;
        c_total += ce;
        m_bands.push(me);
        c_bands.push(ce);
    }
    for (i, &(_, _, name)) in bands.iter().enumerate() {
        let m_pct = m_bands[i] / m_total * 100.0;
        let c_pct = c_bands[i] / c_total * 100.0;
        eprintln!("  {}: mic={:.1}%, cable={:.1}% (diff={:.1}pp)", name, m_pct, c_pct, c_pct - m_pct);
    }

    // === Verdict ===
    eprintln!("\n=== VERDICT ===");
    let mut issues = vec![];
    if clip_pct > 0.1 { issues.push(format!("CLIPPING: {:.1}% samples clipped", clip_pct)); }
    if c_drms_norm / m_drms_norm > 1.5 { issues.push(format!("ROUGHNESS: cable {:.1}x rougher (normalized)", c_drms_norm / m_drms_norm)); }
    if c_floor > m_floor * 3.0 && c_floor > 0.001 { issues.push(format!("NOISE: cable floor {:.1}x higher", c_floor / m_floor)); }
    if c_jumps > m_jumps * 3 + 10 { issues.push(format!("GLITCHES: cable has {} jumps vs mic {}", c_jumps, m_jumps)); }

    // Check frequency balance shift
    for (i, &(_, _, name)) in bands.iter().enumerate() {
        let m_pct = m_bands[i] / m_total * 100.0;
        let c_pct = c_bands[i] / c_total * 100.0;
        if (c_pct - m_pct).abs() > 15.0 {
            issues.push(format!("FREQ SHIFT: {} band differs by {:.1}pp", name, c_pct - m_pct));
        }
    }

    if issues.is_empty() {
        eprintln!("PASS — Cable output quality looks comparable to direct mic");
    } else {
        for issue in &issues {
            eprintln!("ISSUE: {}", issue);
        }
    }
}
