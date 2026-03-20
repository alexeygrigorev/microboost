/// End-to-end pipeline test:
/// 1. Feed test_voice.wav through gain+gate+SpscRing (simulating input callback)
/// 2. Real cpal output callback reads from ring and writes to CABLE Input
/// 3. Record from CABLE Output
/// 4. Save both original and recorded for comparison

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use microboost::{noise_gate, SpscRing, RING_SIZE};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

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

fn find_cable_device(host: &cpal::Host, name_contains: &str, input: bool) -> cpal::Device {
    let mut devices: Box<dyn Iterator<Item = cpal::Device>> = if input {
        Box::new(host.input_devices().unwrap())
    } else {
        Box::new(host.output_devices().unwrap())
    };
    devices
        .find(|d| d.name().map(|n| n.to_lowercase().contains(&name_contains.to_lowercase())).unwrap_or(false))
        .expect(&format!("{} not found", name_contains))
}

fn main() {
    std::fs::create_dir_all("tests/.tmp").ok();
    let (voice, voice_rate) = load_wav("tests/fixtures/test_voice.wav");
    println!("Loaded: {} samples, {}Hz, {:.2}s", voice.len(), voice_rate, voice.len() as f64 / voice_rate as f64);

    let host = cpal::default_host();
    let cable_out_dev = find_cable_device(&host, "CABLE Input", false);
    let cable_in_dev = find_cable_device(&host, "CABLE Output", true);

    let out_config = cable_out_dev.default_output_config().unwrap();
    let in_config = cable_in_dev.default_input_config().unwrap();
    let out_rate = out_config.sample_rate().0;
    let out_channels = out_config.channels() as usize;
    let in_channels = in_config.channels() as usize;
    let rate_ratio = voice_rate as f64 / out_rate as f64;

    println!("CABLE Input (play): {}Hz {}ch", out_rate, out_channels);
    println!("CABLE Output (rec): {}Hz {}ch", in_config.sample_rate().0, in_channels);
    println!("Rate ratio: {:.4}", rate_ratio);

    // Pipeline components
    let ring = Arc::new(SpscRing::new(RING_SIZE));
    let gate = Arc::new(Mutex::new(noise_gate::NoiseGate::new()));
    let gain = 1.0f32;

    // Recording buffer
    let recorded = Arc::new(Mutex::new(Vec::<f32>::new()));
    let rec_clone = recorded.clone();
    let running = Arc::new(AtomicBool::new(true));

    // Start recording from CABLE Output
    let r1 = running.clone();
    let rec_stream = cable_in_dev.build_input_stream(
        &in_config.into(),
        move |data: &[f32], _| {
            if !r1.load(Ordering::Relaxed) { return; }
            let mut rec = rec_clone.lock().unwrap();
            for chunk in data.chunks(in_channels) {
                rec.push(chunk[0]);
            }
        },
        |e| eprintln!("Record error: {}", e),
        None,
    ).unwrap();
    rec_stream.play().unwrap();

    // Output stream — reads from ring, writes to CABLE (same as real app)
    let ring_r = ring.clone();
    let mut last_sample: f32 = 0.0;
    let mut frac: f64 = 0.0;
    let underruns = Arc::new(AtomicU64::new(0));
    let underruns2 = underruns.clone();
    let prebuf_samples = out_rate as usize / 50; // 20ms
    let mut prebuffered = false;

    let out_stream = cable_out_dev.build_output_stream(
        &out_config.into(),
        move |data: &mut [f32], _| {
            if !prebuffered {
                if ring_r.available() < prebuf_samples {
                    data.iter_mut().for_each(|s| *s = 0.0);
                    return;
                }
                prebuffered = true;
            }

            for frame in data.chunks_mut(out_channels) {
                let sample = if ring_r.available() > 1 {
                    let s0 = ring_r.peek(0);
                    let s1 = ring_r.peek(1);
                    let t = frac as f32;
                    let interpolated = s0 + (s1 - s0) * t;
                    frac += rate_ratio;
                    while frac >= 1.0 {
                        frac -= 1.0;
                        if ring_r.available() > 0 {
                            ring_r.advance(1);
                        }
                    }
                    last_sample = interpolated;
                    interpolated
                } else {
                    underruns2.fetch_add(1, Ordering::Relaxed);
                    last_sample
                };
                for ch in frame.iter_mut() {
                    *ch = sample;
                }
            }
        },
        |e| eprintln!("Output error: {}", e),
        None,
    ).unwrap();
    out_stream.play().unwrap();

    std::thread::sleep(std::time::Duration::from_millis(100));

    // Feed voice through pipeline (simulating input callback at real-time speed)
    println!("Feeding voice through pipeline...");
    let ring_w = ring.clone();
    let gate_w = gate.clone();
    let chunk_size = 480; // 10ms at 48kHz
    let chunk_duration_us = (chunk_size as u64 * 1_000_000) / voice_rate as u64;

    // Feed data in chunks with spin-wait pacing
    let start = std::time::Instant::now();
    {
        let mut g = gate_w.lock().unwrap();
        for (i, &sample) in voice.iter().enumerate() {
            let boosted = (sample * gain).clamp(-1.0, 1.0);
            let gated = g.process(boosted);
            ring_w.push(gated);

            if (i + 1) % 48000 == 0 {
                println!("  Fed {}s, ring available={}, write={}",
                    (i + 1) / 48000,
                    ring_w.available(),
                    ring_w.write.load(Ordering::Relaxed));
            }

            // Every 480 samples (10ms), spin-wait until real-time catches up
            if (i + 1) % 480 == 0 {
                let target = std::time::Duration::from_nanos(
                    (i + 1) as u64 * 1_000_000_000 / voice_rate as u64
                );
                while start.elapsed() < target {
                    std::hint::spin_loop();
                }
            }
        }
    }

    // Wait for output to drain — data was fed instantly, output plays at real-time speed
    let drain_ms = (voice.len() as u64 * 1000 / voice_rate as u64) + 1000;
    println!("Draining for {:.1}s...", drain_ms as f64 / 1000.0);
    std::thread::sleep(std::time::Duration::from_millis(drain_ms));

    running.store(false, Ordering::Relaxed);
    drop(out_stream);
    std::thread::sleep(std::time::Duration::from_millis(200));
    drop(rec_stream);

    let rec = recorded.lock().unwrap();
    let ur = underruns.load(Ordering::Relaxed);
    println!("Recorded: {} samples ({:.2}s)", rec.len(), rec.len() as f64 / out_rate as f64);
    println!("Underruns: {}", ur);

    // Save files
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: voice_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };

    let mut w1 = hound::WavWriter::create("tests/.tmp/pipeline_original.wav", spec).unwrap();
    for &s in voice.iter() { w1.write_sample(s).unwrap(); }
    w1.finalize().unwrap();

    let spec2 = hound::WavSpec { sample_rate: out_rate, ..spec };
    let mut w2 = hound::WavWriter::create("tests/.tmp/pipeline_output.wav", spec2).unwrap();
    for &s in rec.iter() { w2.write_sample(s).unwrap(); }
    w2.finalize().unwrap();

    // Analysis
    let rms = |d: &[f32]| (d.iter().map(|s| s * s).sum::<f32>() / d.len() as f32).sqrt();
    let v_rms = rms(&voice);
    let r_rms = rms(&rec);
    println!("\nOriginal RMS: {:.4} ({:.1} dBFS)", v_rms, 20.0 * v_rms.log10());
    println!("Output   RMS: {:.4} ({:.1} dBFS)", r_rms, 20.0 * r_rms.log10());
    println!("Ratio: {:.2}x", r_rms / v_rms);

    println!("\nSaved:");
    println!("  tests/.tmp/pipeline_original.wav  (input)");
    println!("  tests/.tmp/pipeline_output.wav    (after pipeline + CABLE)");
    println!("\nListen and compare!");
}
