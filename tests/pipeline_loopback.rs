/// Send voice through the actual microboost pipeline (ring buffer + gain + gate)
/// into VB-CABLE, record back, and compare with the original.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use microboost::{noise_gate, SpscRing, RING_SIZE};
use std::sync::atomic::Ordering;
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

fn load_wav(path: &str) -> (Vec<f32>, u32) {
    let reader = hound::WavReader::open(path).expect(&format!("failed to open {}", path));
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

#[test]
fn voice_through_pipeline_and_cable() {
    let (voice, voice_rate) = load_wav("tests/test_voice.wav");
    eprintln!(
        "Loaded voice: {} samples, {}Hz, {:.2}s",
        voice.len(),
        voice_rate,
        voice.len() as f64 / voice_rate as f64
    );

    let host = cpal::default_host();
    let cable_out = find_device(&host, "CABLE Input", false)
        .expect("CABLE Input (playback) not found");
    let cable_in = find_device(&host, "CABLE Output", true)
        .expect("CABLE Output (recording) not found");

    let out_config = cable_out.default_output_config().unwrap();
    let in_config = cable_in.default_input_config().unwrap();
    let out_rate = out_config.sample_rate().0;
    let out_channels = out_config.channels() as usize;
    let in_channels = in_config.channels() as usize;

    eprintln!(
        "CABLE out: {}Hz {}ch, CABLE in: {}Hz {}ch",
        out_rate, out_channels, in_config.sample_rate().0, in_channels
    );

    // === Set up the microboost pipeline ===
    let ring = Arc::new(SpscRing::new(RING_SIZE));
    let gate = Arc::new(Mutex::new(noise_gate::NoiseGate::new())); // disabled
    let gain = 1.0f32; // 1x boost

    // Simulate input callback: process voice samples and push to ring
    let ring_w = ring.clone();
    let gate_w = gate.clone();
    let write_pos = Arc::new(Mutex::new(0usize));
    let signal = voice.clone();

    // Record from CABLE Output
    let recorded = Arc::new(Mutex::new(Vec::<f32>::new()));
    let rec_clone = recorded.clone();

    let in_stream_config: cpal::StreamConfig = in_config.into();
    let rec_stream = cable_in
        .build_input_stream(
            &in_stream_config,
            move |data: &[f32], _| {
                let mut rec = rec_clone.lock().unwrap();
                for chunk in data.chunks(in_channels) {
                    rec.push(chunk[0]);
                }
            },
            |e| eprintln!("Record error: {}", e),
            None,
        )
        .unwrap();
    rec_stream.play().unwrap();

    std::thread::sleep(std::time::Duration::from_millis(200));

    // Simulated "input stream" — reads from our WAV and pushes through pipeline
    let feeder = std::thread::spawn(move || {
        let chunk_size = 480; // 10ms at 48kHz
        for chunk in signal.chunks(chunk_size) {
            let mut g = gate_w.lock().unwrap();
            for &sample in chunk {
                let boosted = (sample * gain).clamp(-1.0, 1.0);
                let gated = g.process(boosted);
                ring_w.push(gated);
            }
            drop(g);
            // Simulate real-time pacing
            std::thread::sleep(std::time::Duration::from_micros(
                (chunk.len() as u64 * 1_000_000) / voice_rate as u64,
            ));
        }
    });

    // Output stream — reads from ring buffer and writes to CABLE
    let ring_r = ring.clone();
    let mut last_sample: f32 = 0.0;
    let mut frac: f64 = 0.0;
    let rate_ratio = voice_rate as f64 / out_rate as f64;
    let prebuf_samples = out_rate as usize / 50; // 20ms
    let mut prebuffered = false;
    let underruns = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let underruns2 = underruns.clone();

    let out_stream_config: cpal::StreamConfig = out_config.into();
    let play_stream = cable_out
        .build_output_stream(
            &out_stream_config,
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
            |e| eprintln!("Play error: {}", e),
            None,
        )
        .unwrap();
    play_stream.play().unwrap();

    // Wait for feeder to finish + margin
    feeder.join().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));

    drop(play_stream);
    std::thread::sleep(std::time::Duration::from_millis(300));
    drop(rec_stream);

    let rec = recorded.lock().unwrap();
    let ur = underruns.load(Ordering::Relaxed);
    eprintln!(
        "Recorded {} samples ({:.2}s), underruns: {}",
        rec.len(),
        rec.len() as f64 / out_rate as f64,
        ur
    );

    // Write output
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: out_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };

    let mut w = hound::WavWriter::create("tests/voice_through_pipeline.wav", spec).unwrap();
    for &s in rec.iter() {
        w.write_sample(s).unwrap();
    }
    w.finalize().unwrap();

    eprintln!("\nWrote tests/voice_through_pipeline.wav");
    eprintln!("Compare with tests/voice_original.wav");
}
