/// Deep sample-level comparison of e2e_original.wav vs e2e_ring.wav.
/// Cross-correlate to align, then compare sample-by-sample.

fn load_wav(path: &str) -> (Vec<f32>, u32) {
    let reader = hound::WavReader::open(path).unwrap();
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
    if s.is_empty() { return 0.0; }
    (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt()
}

/// Find the offset in `b` where `a` best aligns (cross-correlation)
fn find_alignment(a: &[f32], b: &[f32], max_offset: usize) -> (usize, f32) {
    let check_len = a.len().min(48000); // check first 1 second
    let mut best_offset = 0;
    let mut best_corr = f32::MIN;

    for offset in 0..max_offset.min(b.len()) {
        let len = check_len.min(b.len() - offset);
        let mut corr = 0.0f32;
        for i in 0..len {
            corr += a[i] * b[offset + i];
        }
        if corr > best_corr {
            best_corr = corr;
            best_offset = offset;
        }
    }
    (best_offset, best_corr)
}

#[test]
fn deep_sample_comparison() {
    let (orig, or) = load_wav("tests/.tmp/e2e_original.wav");
    let (ring, rr) = load_wav("tests/.tmp/e2e_ring.wav");

    eprintln!("Original: {} samples ({}Hz)", orig.len(), or);
    eprintln!("Ring out: {} samples ({}Hz)", ring.len(), rr);

    // Find alignment (ring output has startup delay from pre-buffer + CABLE latency)
    let (offset, corr) = find_alignment(&orig, &ring, 48000);
    eprintln!("Best alignment: ring offset={} samples ({:.1}ms), correlation={:.2}",
        offset, offset as f64 / rr as f64 * 1000.0, corr);

    // Align signals
    let compare_len = orig.len().min(ring.len() - offset);
    let a = &orig[..compare_len];
    let b = &ring[offset..offset + compare_len];

    // Normalize both to same RMS for fair comparison
    let a_rms = rms(a);
    let b_rms = rms(b);
    eprintln!("Aligned RMS: orig={:.4}, ring={:.4}, ratio={:.3}", a_rms, b_rms, b_rms / a_rms);

    let scale = if b_rms > 0.0 { a_rms / b_rms } else { 1.0 };

    // Sample-by-sample error
    let mut sum_err_sq = 0.0f64;
    let mut sum_sig_sq = 0.0f64;
    let mut max_err: f32 = 0.0;
    let mut max_err_idx = 0;
    let mut err_samples = 0; // samples with >1% error

    for i in 0..compare_len {
        let expected = a[i];
        let actual = b[i] * scale;
        let err = (expected - actual).abs();
        sum_err_sq += (err as f64) * (err as f64);
        sum_sig_sq += (expected as f64) * (expected as f64);
        if err > max_err {
            max_err = err;
            max_err_idx = i;
        }
        if expected.abs() > 0.01 && err / expected.abs() > 0.01 {
            err_samples += 1;
        }
    }

    let snr = if sum_err_sq > 0.0 {
        10.0 * (sum_sig_sq / sum_err_sq).log10()
    } else {
        f64::INFINITY
    };
    let rmse = (sum_err_sq / compare_len as f64).sqrt();

    eprintln!("\n=== Sample-level comparison ({} samples) ===", compare_len);
    eprintln!("SNR: {:.1} dB", snr);
    eprintln!("RMSE: {:.6}", rmse);
    eprintln!("Max error: {:.6} at sample {} ({:.1}ms)", max_err, max_err_idx, max_err_idx as f64 / or as f64 * 1000.0);
    eprintln!("Samples with >1% error: {} ({:.2}%)", err_samples, err_samples as f64 / compare_len as f64 * 100.0);

    // Check for dropped/inserted samples by looking at local correlation
    let window = 480; // 10ms
    let mut bad_windows = 0;
    let mut total_windows = 0;
    for start in (0..compare_len - window).step_by(window) {
        let aw = &a[start..start + window];
        let bw: Vec<f32> = b[start..start + window].iter().map(|&x| x * scale).collect();
        let mut corr = 0.0f32;
        let mut a_energy = 0.0f32;
        let mut b_energy = 0.0f32;
        for i in 0..window {
            corr += aw[i] * bw[i];
            a_energy += aw[i] * aw[i];
            b_energy += bw[i] * bw[i];
        }
        let norm = (a_energy * b_energy).sqrt();
        let normalized_corr = if norm > 0.0 { corr / norm } else { 1.0 };
        total_windows += 1;
        if normalized_corr < 0.9 && rms(aw) > 0.01 {
            bad_windows += 1;
            if bad_windows <= 5 {
                eprintln!("  Bad window at {:.1}ms: correlation={:.3}, rms={:.4}",
                    start as f64 / or as f64 * 1000.0, normalized_corr, rms(aw));
            }
        }
    }
    eprintln!("Bad windows (<0.9 correlation): {}/{} ({:.1}%)",
        bad_windows, total_windows, bad_windows as f64 / total_windows as f64 * 100.0);

    // Frequency response check
    let bands = [
        (80.0, 300.0, "Sub/Low"),
        (300.0, 1000.0, "Low-Mid"),
        (1000.0, 3000.0, "Mid"),
        (3000.0, 8000.0, "High-Mid"),
    ];

    fn band_energy(samples: &[f32], rate: u32, lo: f32, hi: f32) -> f32 {
        let n = samples.len().min(4096);
        let mut e = 0.0f64;
        let bin_lo = (lo * n as f32 / rate as f32) as usize;
        let bin_hi = (hi * n as f32 / rate as f32) as usize;
        for bin in bin_lo..=bin_hi.min(n / 2) {
            let mut real = 0.0f64;
            let mut imag = 0.0f64;
            for (i, &s) in samples[..n].iter().enumerate() {
                let angle = 2.0 * std::f64::consts::PI * bin as f64 * i as f64 / n as f64;
                real += s as f64 * angle.cos();
                imag += s as f64 * (-angle.sin());
            }
            e += real * real + imag * imag;
        }
        e as f32
    }

    // Find a voiced segment
    let seg_len = 4096;
    let mut best_start = 0;
    let mut best_rms = 0.0f32;
    for start in (0..a.len() - seg_len).step_by(seg_len / 2) {
        let r = rms(&a[start..start + seg_len]);
        if r > best_rms { best_rms = r; best_start = start; }
    }

    let a_seg = &a[best_start..best_start + seg_len];
    let b_seg: Vec<f32> = b[best_start..best_start + seg_len].iter().map(|&x| x * scale).collect();

    eprintln!("\n=== Frequency response (loudest segment at {:.1}ms) ===", best_start as f64 / or as f64 * 1000.0);
    let mut a_total = 0.0f32;
    let mut b_total = 0.0f32;
    let mut a_e = vec![];
    let mut b_e = vec![];
    for &(lo, hi, _) in &bands {
        let ae = band_energy(a_seg, or, lo, hi);
        let be = band_energy(&b_seg, rr, lo, hi);
        a_e.push(ae); b_e.push(be);
        a_total += ae; b_total += be;
    }
    for (i, &(_, _, name)) in bands.iter().enumerate() {
        let ap = a_e[i] / a_total * 100.0;
        let bp = b_e[i] / b_total * 100.0;
        eprintln!("  {}: orig={:.1}% ring={:.1}% diff={:+.1}pp", name, ap, bp, bp - ap);
    }

    // === FINAL VERDICT ===
    eprintln!("\n=============================");
    let bad_pct = bad_windows as f64 / total_windows as f64;
    let gain_ratio = b_rms / a_rms;
    let pass = snr > 30.0 && bad_pct < 0.05 && (gain_ratio - 1.0).abs() < 0.2;

    if pass {
        eprintln!("VERDICT: PASS — ring buffer output matches original");
        eprintln!("  SNR={:.1}dB, {:.1}% bad windows, gain={:.2}x",
            snr, bad_windows as f64 / total_windows as f64 * 100.0, b_rms / a_rms);
    } else {
        eprintln!("VERDICT: FAIL — quality issues detected");
        if snr <= 30.0 { eprintln!("  SNR too low: {:.1}dB", snr); }
        if bad_windows as f64 / total_windows as f64 >= 0.05 {
            eprintln!("  Too many bad windows: {:.1}%", bad_windows as f64 / total_windows as f64 * 100.0);
        }
        if (b_rms / a_rms - 1.0).abs() >= 0.2 {
            eprintln!("  Gain mismatch: {:.2}x", b_rms / a_rms);
        }
    }
    eprintln!("=============================");

    // Also compare ring vs direct (bypasses CABLE noise)
    let (direct, dr2) = load_wav("tests/.tmp/e2e_direct.wav");
    let (ring2, _) = load_wav("tests/.tmp/e2e_ring.wav");
    let (off2, _) = find_alignment(&direct, &ring2, 48000);
    let clen2 = direct.len().min(ring2.len() - off2);
    let d_seg = &direct[..clen2];
    let r_seg = &ring2[off2..off2 + clen2];
    let d_rms2 = rms(d_seg);
    let r_rms2 = rms(r_seg);
    let scale2 = if r_rms2 > 0.0 { d_rms2 / r_rms2 } else { 1.0 };
    let mut se2 = 0.0f64;
    let mut ss2 = 0.0f64;
    for i in 0..clen2 {
        let e = d_seg[i] - r_seg[i] * scale2;
        se2 += (e as f64) * (e as f64);
        ss2 += (d_seg[i] as f64) * (d_seg[i] as f64);
    }
    let snr2 = if se2 > 0.0 { 10.0 * (ss2 / se2).log10() } else { 999.0 };
    eprintln!("\n=== Ring vs Direct (isolates ring buffer from CABLE noise) ===");
    eprintln!("Alignment offset: {} samples ({:.1}ms)", off2, off2 as f64 / dr2 as f64 * 1000.0);
    eprintln!("SNR: {:.1} dB (>40 = transparent)", snr2);
    eprintln!("Gain: {:.3}x", r_rms2 / d_rms2);
}
