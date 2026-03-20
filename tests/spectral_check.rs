/// Compare full spectral content of mic vs cable recordings.
/// Uses overlapping windows across the entire recording.

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

fn rms(s: &[f32]) -> f32 {
    (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt()
}

/// Average spectral energy across many overlapping windows
fn avg_band_energy(samples: &[f32], rate: u32, lo: f32, hi: f32) -> f32 {
    let n = 4096;
    let hop = n / 2;
    let mut total = 0.0f64;
    let mut count = 0;

    let mut pos = 0;
    while pos + n <= samples.len() {
        // Only analyze windows with significant audio (skip silence)
        let w = &samples[pos..pos + n];
        if rms(w) > 0.005 {
            let bin_lo = (lo * n as f32 / rate as f32) as usize;
            let bin_hi = (hi * n as f32 / rate as f32) as usize;
            for bin in bin_lo..=bin_hi.min(n / 2) {
                let mut real = 0.0f64;
                let mut imag = 0.0f64;
                for (i, &s) in w.iter().enumerate() {
                    let angle = 2.0 * std::f64::consts::PI * bin as f64 * i as f64 / n as f64;
                    real += s as f64 * angle.cos();
                    imag += s as f64 * (-angle.sin());
                }
                total += real * real + imag * imag;
            }
            count += 1;
        }
        pos += hop;
    }

    if count > 0 { (total / count as f64) as f32 } else { 0.0 }
}

#[test]
fn spectral_comparison() {
    let (mic, mr) = load_wav("tests/.tmp/dual_mic.wav");
    let (cable, cr) = load_wav("tests/.tmp/dual_cable.wav");

    eprintln!("Analyzing full recordings (voiced segments only)...");
    eprintln!("Mic: {} samples, Cable: {} samples", mic.len(), cable.len());

    let bands = [
        (80.0, 300.0, "Sub/Low (80-300Hz)"),
        (300.0, 1000.0, "Low-Mid (300-1kHz)"),
        (1000.0, 3000.0, "Mid (1-3kHz) — voice clarity"),
        (3000.0, 8000.0, "High-Mid (3-8kHz) — presence"),
        (8000.0, 16000.0, "High (8-16kHz) — air"),
    ];

    let mut m_energies = vec![];
    let mut c_energies = vec![];
    let mut m_total = 0.0f32;
    let mut c_total = 0.0f32;

    for &(lo, hi, _) in &bands {
        let me = avg_band_energy(&mic, mr, lo, hi);
        let ce = avg_band_energy(&cable, cr, lo, hi);
        m_energies.push(me);
        c_energies.push(ce);
        m_total += me;
        c_total += ce;
    }

    eprintln!("\n{:<35} {:>8} {:>8} {:>8}", "Band", "Mic %", "Cable %", "Diff");
    eprintln!("{}", "-".repeat(65));
    for (i, &(_, _, name)) in bands.iter().enumerate() {
        let m_pct = if m_total > 0.0 { m_energies[i] / m_total * 100.0 } else { 0.0 };
        let c_pct = if c_total > 0.0 { c_energies[i] / c_total * 100.0 } else { 0.0 };
        let diff = c_pct - m_pct;
        let flag = if diff.abs() > 10.0 { " ⚠" } else { "" };
        eprintln!("{:<35} {:>7.1}% {:>7.1}% {:>+7.1}pp{}", name, m_pct, c_pct, diff, flag);
    }

    // Overall gain ratio
    let m_rms = rms(&mic);
    let c_rms = rms(&cable);
    eprintln!("\nOverall gain: {:.2}x ({:+.1}dB)", c_rms / m_rms, 20.0 * (c_rms / m_rms).log10());

    // Verdict
    eprintln!("\n=== VERDICT ===");
    let mut ok = true;
    for (i, &(_, _, name)) in bands.iter().enumerate() {
        let m_pct = m_energies[i] / m_total * 100.0;
        let c_pct = c_energies[i] / c_total * 100.0;
        if (c_pct - m_pct).abs() > 10.0 {
            eprintln!("WARNING: {} band shifted by {:.1}pp", name, c_pct - m_pct);
            ok = false;
        }
    }
    if ok {
        eprintln!("PASS — frequency response is preserved through pipeline");
    }
}
