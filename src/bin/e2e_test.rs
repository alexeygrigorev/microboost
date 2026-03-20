/// End-to-end pipeline test.
/// Feeds WAV through ring buffer -> cpal output callback -> CABLE -> record back.
/// Uses Windows multimedia timer for accurate pacing.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use microboost::{SpscRing, RING_SIZE};
use std::sync::{Arc, Mutex};

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

fn find_device(host: &cpal::Host, name_contains: &str, input: bool) -> cpal::Device {
    let mut devices: Box<dyn Iterator<Item = cpal::Device>> = if input {
        Box::new(host.input_devices().unwrap())
    } else {
        Box::new(host.output_devices().unwrap())
    };
    devices
        .find(|d| d.name().map(|n| n.to_lowercase().contains(&name_contains.to_lowercase())).unwrap_or(false))
        .expect(&format!("Device '{}' not found", name_contains))
}

fn avg_band_energy(samples: &[f32], rate: u32, lo: f32, hi: f32) -> f32 {
    let n = 4096;
    let hop = n / 2;
    let mut total = 0.0f64;
    let mut count = 0;
    let mut pos = 0;
    while pos + n <= samples.len() {
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

fn main() {
    std::fs::create_dir_all("tests/.tmp").ok();
    // Step 1: Send voice directly to CABLE (no ring buffer) as baseline
    let (voice, voice_rate) = load_wav("tests/test_voice.wav");
    println!("Voice: {} samples, {}Hz, {:.2}s", voice.len(), voice_rate, voice.len() as f64 / voice_rate as f64);

    let host = cpal::default_host();
    let cable_out_dev = find_device(&host, "CABLE Input", false);
    let cable_in_dev = find_device(&host, "CABLE Output", true);

    let out_config = cable_out_dev.default_output_config().unwrap();
    let rec_config = cable_in_dev.default_input_config().unwrap();
    let out_rate = out_config.sample_rate().0;
    let out_channels = out_config.channels() as usize;
    let rec_channels = rec_config.channels() as usize;

    println!("CABLE out: {}Hz {}ch, rec: {}Hz {}ch", out_rate, out_channels, rec_config.sample_rate().0, rec_channels);

    // ======= TEST A: Direct to CABLE (baseline, no ring buffer) =======
    println!("\n=== TEST A: Direct to CABLE (no ring buffer) ===");
    let recorded_a = run_direct(&voice, &cable_out_dev, &out_config, &cable_in_dev, &rec_config, out_channels, rec_channels);

    // ======= TEST B: Through ring buffer (same as app) =======
    println!("\n=== TEST B: Through ring buffer (same as app pipeline) ===");
    let recorded_b = run_with_ring(&voice, voice_rate, &cable_out_dev, &out_config, &cable_in_dev, &rec_config, out_channels, rec_channels);

    // Save all files
    let spec = hound::WavSpec {
        channels: 1, sample_rate: out_rate,
        bits_per_sample: 32, sample_format: hound::SampleFormat::Float,
    };
    save_wav("tests/.tmp/e2e_original.wav", &voice, spec);
    save_wav("tests/.tmp/e2e_direct.wav", &recorded_a, spec);
    save_wav("tests/.tmp/e2e_ring.wav", &recorded_b, spec);

    // Compare
    println!("\n=== COMPARISON ===");
    analyze("Original", &voice, voice_rate);
    analyze("Direct (no ring)", &recorded_a, out_rate);
    analyze("Ring buffer", &recorded_b, out_rate);

    compare("Direct vs Original", &voice, &recorded_a, voice_rate, out_rate);
    compare("Ring vs Original", &voice, &recorded_b, voice_rate, out_rate);
    compare("Ring vs Direct", &recorded_a, &recorded_b, out_rate, out_rate);
}

fn save_wav(path: &str, data: &[f32], spec: hound::WavSpec) {
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for &s in data { w.write_sample(s).unwrap(); }
    w.finalize().unwrap();
}

fn analyze(name: &str, data: &[f32], rate: u32) {
    let r = rms(data);
    let peak = data.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
    println!("  {}: {} samples, RMS={:.4} ({:.1}dB), peak={:.4}", name, data.len(), r, 20.0 * r.log10(), peak);
}

fn compare(name: &str, a: &[f32], b: &[f32], rate_a: u32, rate_b: u32) {
    let bands = [(80.0,300.0,"Sub"),(300.0,1000.0,"LMid"),(1000.0,3000.0,"Mid"),(3000.0,8000.0,"HMid"),(8000.0,16000.0,"High")];
    let mut max_shift = 0.0f32;
    let mut a_total = 0.0f32;
    let mut b_total = 0.0f32;
    let mut a_e = vec![];
    let mut b_e = vec![];
    for &(lo, hi, _) in &bands {
        let ae = avg_band_energy(a, rate_a, lo, hi);
        let be = avg_band_energy(b, rate_b, lo, hi);
        a_e.push(ae); b_e.push(be);
        a_total += ae; b_total += be;
    }
    print!("  {}: ", name);
    for (i, &(_, _, nm)) in bands.iter().enumerate() {
        let ap = if a_total > 0.0 { a_e[i] / a_total * 100.0 } else { 0.0 };
        let bp = if b_total > 0.0 { b_e[i] / b_total * 100.0 } else { 0.0 };
        let d = bp - ap;
        max_shift = max_shift.max(d.abs());
        if d.abs() > 5.0 { print!("{}={:+.0}pp⚠ ", nm, d); }
    }
    let ra = rms(a);
    let rb = rms(b);
    let gain = if ra > 0.0 { rb / ra } else { 0.0 };
    let da = if a.len() > 1 { (1..a.len()).map(|i| { let x = a[i]-a[i-1]; x*x }).sum::<f32>() / a.len() as f32 } else { 0.0 };
    let db = if b.len() > 1 { (1..b.len()).map(|i| { let x = b[i]-b[i-1]; x*x }).sum::<f32>() / b.len() as f32 } else { 0.0 };
    let rough = if da > 0.0 { (db / da).sqrt() } else { 0.0 };
    print!("gain={:.2}x rough={:.2}x maxΔ={:.0}pp", gain, rough, max_shift);
    if max_shift < 10.0 && (gain - 1.0).abs() < 0.3 && rough < 1.3 {
        println!(" ✓");
    } else {
        println!(" ✗");
    }
}

fn run_direct(voice: &[f32], cable_out: &cpal::Device, out_cfg: &cpal::SupportedStreamConfig,
              cable_in: &cpal::Device, rec_cfg: &cpal::SupportedStreamConfig,
              out_ch: usize, rec_ch: usize) -> Vec<f32> {
    let recorded = Arc::new(Mutex::new(Vec::<f32>::new()));
    let rec = recorded.clone();
    let write_pos = Arc::new(Mutex::new(0usize));
    let signal = voice.to_vec();

    let rec_stream = cable_in.build_input_stream(
        &rec_cfg.clone().into(),
        move |data: &[f32], _| {
            let mut r = rec.lock().unwrap();
            for chunk in data.chunks(rec_ch) { r.push(chunk[0]); }
        },
        |e| eprintln!("Rec error: {}", e), None,
    ).unwrap();
    rec_stream.play().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));

    let play_stream = cable_out.build_output_stream(
        &out_cfg.clone().into(),
        move |data: &mut [f32], _| {
            let mut pos = write_pos.lock().unwrap();
            for frame in data.chunks_mut(out_ch) {
                let s = if *pos < signal.len() { signal[*pos] } else { 0.0 };
                *pos += 1;
                for ch in frame.iter_mut() { *ch = s; }
            }
        },
        |e| eprintln!("Play error: {}", e), None,
    ).unwrap();
    play_stream.play().unwrap();

    let dur = std::time::Duration::from_millis(voice.len() as u64 * 1000 / 48000 + 500);
    std::thread::sleep(dur);
    drop(play_stream);
    std::thread::sleep(std::time::Duration::from_millis(200));
    drop(rec_stream);

    Arc::try_unwrap(recorded).unwrap().into_inner().unwrap()
}

fn run_with_ring(voice: &[f32], voice_rate: u32,
                 cable_out: &cpal::Device, out_cfg: &cpal::SupportedStreamConfig,
                 cable_in: &cpal::Device, rec_cfg: &cpal::SupportedStreamConfig,
                 out_ch: usize, rec_ch: usize) -> Vec<f32> {
    let ring = Arc::new(SpscRing::new(RING_SIZE));

    let recorded = Arc::new(Mutex::new(Vec::<f32>::new()));
    let rec = recorded.clone();

    // Record from CABLE
    let rec_stream = cable_in.build_input_stream(
        &rec_cfg.clone().into(),
        move |data: &[f32], _| {
            let mut r = rec.lock().unwrap();
            for chunk in data.chunks(rec_ch) { r.push(chunk[0]); }
        },
        |e| eprintln!("Rec error: {}", e), None,
    ).unwrap();
    rec_stream.play().unwrap();

    // Output callback — reads from ring (same as real app, lock-free)
    let ring_r = ring.clone();

    let out_stream = cable_out.build_output_stream(
        &out_cfg.clone().into(),
        move |data: &mut [f32], _| {
            for frame in data.chunks_mut(out_ch) {
                let sample = if ring_r.available() > 0 {
                    let s = ring_r.peek(0);
                    ring_r.advance(1);
                    s
                } else {
                    0.0
                };
                for ch in frame.iter_mut() { *ch = sample; }
            }
        },
        |e| eprintln!("Out error: {}", e), None,
    ).unwrap();
    out_stream.play().unwrap();

    std::thread::sleep(std::time::Duration::from_millis(100));

    // Feed voice into ring at real-time rate using spin-wait
    let ring_w = ring.clone();
    let start = std::time::Instant::now();
    let chunk_size = 480; // 10ms

    for (ci, chunk) in voice.chunks(chunk_size).enumerate() {
        for &sample in chunk {
            ring_w.push(sample.clamp(-1.0, 1.0));
        }
        // Spin-wait until real-time catches up
        let target = std::time::Duration::from_nanos(
            ((ci + 1) * chunk_size) as u64 * 1_000_000_000 / voice_rate as u64
        );
        while start.elapsed() < target {
            std::hint::spin_loop();
        }
    }

    // Drain
    std::thread::sleep(std::time::Duration::from_millis(500));
    drop(out_stream);
    std::thread::sleep(std::time::Duration::from_millis(200));
    drop(rec_stream);

    Arc::try_unwrap(recorded).unwrap().into_inner().unwrap()
}
