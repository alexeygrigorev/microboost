/// Compare direct mic recording vs cable output recording to analyze noise.

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
    let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
    (sum_sq / samples.len() as f32).sqrt()
}

fn rms_db(samples: &[f32]) -> f32 {
    let r = rms(samples);
    if r > 0.0 { 20.0 * r.log10() } else { -100.0 }
}

/// Compute spectrum using simple DFT of first N samples
fn peak_frequency(samples: &[f32], sample_rate: u32) -> (f32, f32) {
    let n = samples.len().min(4096);
    let mut max_mag = 0.0f32;
    let mut max_freq = 0.0f32;

    // Check frequencies from 20Hz to 20kHz
    for freq_bin in 1..n/2 {
        let freq = freq_bin as f32 * sample_rate as f32 / n as f32;
        if freq < 20.0 || freq > 20000.0 { continue; }

        let mut real = 0.0f32;
        let mut imag = 0.0f32;
        for (i, &s) in samples[..n].iter().enumerate() {
            let angle = 2.0 * std::f32::consts::PI * freq_bin as f32 * i as f32 / n as f32;
            real += s * angle.cos();
            imag += s * -angle.sin();
        }
        let mag = (real * real + imag * imag).sqrt();
        if mag > max_mag {
            max_mag = mag;
            max_freq = freq;
        }
    }
    (max_freq, max_mag)
}

#[test]
fn analyze_noise_difference() {
    let (mic, mic_rate) = load_wav("tests/rec_mic_direct.wav");
    let (cable, cable_rate) = load_wav("tests/rec_cable_output.wav");

    eprintln!("=== Direct mic ===");
    eprintln!("  Samples: {}, Rate: {}Hz", mic.len(), mic_rate);
    eprintln!("  RMS: {:.6} ({:.1} dBFS)", rms(&mic), rms_db(&mic));
    eprintln!("  Peak sample: {:.6}", mic.iter().map(|s| s.abs()).fold(0.0f32, f32::max));
    let (mf, mm) = peak_frequency(&mic, mic_rate);
    eprintln!("  Peak frequency: {:.0}Hz (mag={:.2})", mf, mm);

    eprintln!("\n=== Cable output ===");
    eprintln!("  Samples: {}, Rate: {}Hz", cable.len(), cable_rate);
    eprintln!("  RMS: {:.6} ({:.1} dBFS)", rms(&cable), rms_db(&cable));
    eprintln!("  Peak sample: {:.6}", cable.iter().map(|s| s.abs()).fold(0.0f32, f32::max));
    let (cf, cm) = peak_frequency(&cable, cable_rate);
    eprintln!("  Peak frequency: {:.0}Hz (mag={:.2})", cf, cm);

    // Analyze noise floor: look at the quietest 10% of windows
    let window_size = 480; // 10ms
    let mut mic_windows: Vec<f32> = mic.chunks(window_size)
        .map(|w| rms(w))
        .collect();
    let mut cable_windows: Vec<f32> = cable.chunks(window_size)
        .map(|w| rms(w))
        .collect();
    mic_windows.sort_by(|a, b| a.partial_cmp(b).unwrap());
    cable_windows.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let quiet_10_mic = mic_windows.len() / 10;
    let quiet_10_cable = cable_windows.len() / 10;
    let mic_floor = if quiet_10_mic > 0 {
        mic_windows[..quiet_10_mic].iter().sum::<f32>() / quiet_10_mic as f32
    } else { 0.0 };
    let cable_floor = if quiet_10_cable > 0 {
        cable_windows[..quiet_10_cable].iter().sum::<f32>() / quiet_10_cable as f32
    } else { 0.0 };

    eprintln!("\n=== Noise floor (quietest 10% of 10ms windows) ===");
    eprintln!("  Mic:   {:.6} ({:.1} dBFS)", mic_floor, if mic_floor > 0.0 { 20.0 * mic_floor.log10() } else { -100.0 });
    eprintln!("  Cable: {:.6} ({:.1} dBFS)", cable_floor, if cable_floor > 0.0 { 20.0 * cable_floor.log10() } else { -100.0 });
    if cable_floor > 0.0 && mic_floor > 0.0 {
        eprintln!("  Cable noise floor is {:.1}x higher", cable_floor / mic_floor);
    }

    // Look for periodic patterns in cable signal (could indicate clock issues)
    eprintln!("\n=== Sample-level analysis (first 100 samples) ===");
    let compare_len = mic.len().min(cable.len()).min(100);
    for i in 0..compare_len {
        if i < 20 || (mic[i] - cable[i]).abs() > 0.01 {
            eprintln!("  [{}] mic={:.6}  cable={:.6}  diff={:.6}", i, mic[i], cable[i], mic[i] - cable[i]);
        }
    }

    // Check for zero-crossings (clicks would show as sudden jumps)
    let mut cable_jumps = 0;
    for i in 1..cable.len() {
        let diff = (cable[i] - cable[i-1]).abs();
        if diff > 0.1 {
            cable_jumps += 1;
        }
    }
    let mut mic_jumps = 0;
    for i in 1..mic.len() {
        let diff = (mic[i] - mic[i-1]).abs();
        if diff > 0.1 {
            mic_jumps += 1;
        }
    }
    eprintln!("\n=== Large sample jumps (>0.1) ===");
    eprintln!("  Mic:   {}", mic_jumps);
    eprintln!("  Cable: {}", cable_jumps);
}
