/// Loopback test: write a known sine wave to CABLE Input (playback),
/// record from CABLE Output (recording), compare.
/// This tests VB-CABLE independently from microboost.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{Arc, Mutex};

fn find_device(host: &cpal::Host, name_contains: &str, input: bool) -> Option<cpal::Device> {
    let devices = if input {
        host.input_devices().ok()
    } else {
        host.output_devices().ok()
    };
    devices?.find(|d| {
        d.name()
            .map(|n| n.to_lowercase().contains(&name_contains.to_lowercase()))
            .unwrap_or(false)
    })
}

#[test]
fn cable_loopback_sine() {
    let host = cpal::default_host();

    // Find CABLE devices
    let cable_out_dev = find_device(&host, "CABLE Input", false)
        .expect("CABLE Input (playback) not found");
    let cable_in_dev = find_device(&host, "CABLE Output", true)
        .expect("CABLE Output (recording) not found");

    let out_config = cable_out_dev.default_output_config().unwrap();
    let in_config = cable_in_dev.default_input_config().unwrap();

    eprintln!("CABLE Input (playback):  {}Hz {}ch {:?}",
        out_config.sample_rate().0, out_config.channels(), out_config.sample_format());
    eprintln!("CABLE Output (recording): {}Hz {}ch {:?}",
        in_config.sample_rate().0, in_config.channels(), in_config.sample_format());

    let out_rate = out_config.sample_rate().0;
    let out_channels = out_config.channels() as usize;
    let in_channels = in_config.channels() as usize;

    // Generate 1 second of 440Hz sine at 0.5 amplitude
    let duration = 1.0f64;
    let total_frames = (out_rate as f64 * duration) as usize;
    let freq = 440.0f64;
    let test_signal: Vec<f32> = (0..total_frames)
        .map(|i| (2.0 * std::f64::consts::PI * freq * i as f64 / out_rate as f64).sin() as f32 * 0.5)
        .collect();

    // Shared state
    let recorded_l = Arc::new(Mutex::new(Vec::<f32>::new()));
    let recorded_r = Arc::new(Mutex::new(Vec::<f32>::new()));
    let rec_l_clone = recorded_l.clone();
    let rec_r_clone = recorded_r.clone();
    let write_pos = Arc::new(Mutex::new(0usize));
    let signal = test_signal.clone();

    let in_stream_config: cpal::StreamConfig = in_config.into();
    let rec_stream = cable_in_dev.build_input_stream(
        &in_stream_config,
        move |data: &[f32], _| {
            let mut rec_l = rec_l_clone.lock().unwrap();
            let mut rec_r = rec_r_clone.lock().unwrap();
            for chunk in data.chunks(in_channels) {
                rec_l.push(chunk[0]);
                if in_channels > 1 {
                    rec_r.push(chunk[1]);
                }
            }
        },
        |e| eprintln!("Record error: {}", e),
        None,
    ).unwrap();
    rec_stream.play().unwrap();

    // Small delay to let recording start
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Start playback
    let out_stream_config: cpal::StreamConfig = out_config.into();
    let play_stream = cable_out_dev.build_output_stream(
        &out_stream_config,
        move |data: &mut [f32], _| {
            let mut pos = write_pos.lock().unwrap();
            for frame in data.chunks_mut(out_channels) {
                let sample = if *pos < signal.len() {
                    signal[*pos]
                } else {
                    0.0
                };
                *pos += 1;
                for ch in frame.iter_mut() {
                    *ch = sample;
                }
            }
        },
        |e| eprintln!("Play error: {}", e),
        None,
    ).unwrap();
    play_stream.play().unwrap();

    // Wait for playback to complete + margin
    std::thread::sleep(std::time::Duration::from_millis(1500));

    // Stop
    drop(play_stream);
    std::thread::sleep(std::time::Duration::from_millis(200));
    drop(rec_stream);

    let rec = recorded_l.lock().unwrap();
    let rec_r = recorded_r.lock().unwrap();
    eprintln!("\nRecorded L={} R={} samples ({:.2}s)", rec.len(), rec_r.len(), rec.len() as f64 / out_rate as f64);

    // Channel analysis
    if !rec_r.is_empty() {
        let rms_l: f32 = (rec.iter().map(|s| s * s).sum::<f32>() / rec.len() as f32).sqrt();
        let rms_r_val: f32 = (rec_r.iter().map(|s| s * s).sum::<f32>() / rec_r.len() as f32).sqrt();
        eprintln!("L RMS: {:.4} ({:.1} dB)  R RMS: {:.4} ({:.1} dB)",
            rms_l, 20.0 * rms_l.log10(), rms_r_val, 20.0 * rms_r_val.log10());

        // Check if channels differ
        let compare = rec.len().min(rec_r.len()).min(20);
        eprintln!("First {} samples per channel:", compare);
        for i in 0..compare {
            eprintln!("  [{}] L={:.6}  R={:.6}  diff={:.6}", i, rec[i], rec_r[i], rec[i] - rec_r[i]);
        }
    }

    // Skip first 100ms (startup transient) and analyze
    let skip = out_rate as usize / 10;
    if rec.len() < skip + out_rate as usize / 2 {
        panic!("Not enough recorded data: {} samples", rec.len());
    }

    let analysis = &rec[skip..];

    // RMS
    let rms: f32 = (analysis.iter().map(|s| s * s).sum::<f32>() / analysis.len() as f32).sqrt();
    let rms_db = 20.0 * rms.log10();
    eprintln!("Recorded RMS: {:.4} ({:.1} dBFS)", rms, rms_db);
    eprintln!("Expected RMS: {:.4} ({:.1} dBFS)", 0.5 / 2.0f32.sqrt(),
        20.0 * (0.5 / 2.0f32.sqrt()).log10());

    // Peak
    let peak = analysis.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
    eprintln!("Recorded peak: {:.4} (expected ~0.5)", peak);

    // Find the dominant frequency using zero-crossing rate
    let mut zero_crossings = 0;
    for i in 1..analysis.len() {
        if (analysis[i] >= 0.0) != (analysis[i-1] >= 0.0) {
            zero_crossings += 1;
        }
    }
    let detected_freq = zero_crossings as f64 / 2.0 / (analysis.len() as f64 / out_rate as f64);
    eprintln!("Detected frequency: {:.0}Hz (expected 440Hz)", detected_freq);

    // Check for distortion: compare with a clean sine
    // Find phase offset by looking for first positive zero crossing
    let mut phase_start = 0;
    for i in 1..analysis.len().min(out_rate as usize) {
        if analysis[i] >= 0.0 && analysis[i-1] < 0.0 {
            phase_start = i;
            break;
        }
    }

    // Compare recorded vs expected sine from that point
    let compare_len = (out_rate as usize / 2).min(analysis.len() - phase_start);
    let mut sum_diff_sq = 0.0f64;
    let mut sum_sig_sq = 0.0f64;
    for i in 0..compare_len {
        let t = i as f64 / out_rate as f64;
        let expected = (2.0 * std::f64::consts::PI * freq * t).sin() as f32 * peak; // scale to actual peak
        let actual = analysis[phase_start + i];
        let diff = (actual - expected) as f64;
        sum_diff_sq += diff * diff;
        sum_sig_sq += (expected as f64) * (expected as f64);
    }
    let snr_db = 10.0 * (sum_sig_sq / sum_diff_sq).log10();
    let thd_pct = (sum_diff_sq / sum_sig_sq).sqrt() * 100.0;
    eprintln!("\nSignal-to-noise ratio: {:.1} dB", snr_db);
    eprintln!("Total harmonic distortion: {:.2}%", thd_pct);

    // Write recorded signal to WAV for manual inspection
    let out_spec = hound::WavSpec {
        channels: 1,
        sample_rate: out_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create("tests/cable_loopback_recorded.wav", out_spec).unwrap();
    for &s in rec.iter() {
        writer.write_sample(s).unwrap();
    }
    writer.finalize().unwrap();
    eprintln!("\nWrote tests/cable_loopback_recorded.wav for inspection");

    // Write expected signal for comparison
    let mut writer2 = hound::WavWriter::create("tests/cable_loopback_expected.wav", out_spec).unwrap();
    for &s in test_signal.iter() {
        writer2.write_sample(s).unwrap();
    }
    writer2.finalize().unwrap();

    // Assertions — just check signal exists
    assert!(rms > 0.1, "Signal too quiet: RMS={:.4}", rms);
    assert!(peak > 0.3, "Peak too low: {:.4}", peak);

    eprintln!("\nCompare tests/cable_loopback_expected.wav vs tests/cable_loopback_recorded.wav");
    eprintln!("THD={:.2}% SNR={:.1}dB — listen to both files to judge quality", thd_pct, snr_db);
}
