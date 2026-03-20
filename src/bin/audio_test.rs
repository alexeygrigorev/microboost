/// Test tool: lists input devices, records from chosen mic + CABLE Output,
/// saves both to WAV files for comparison.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}};

fn main() {
    let host = cpal::default_host();
    let args: Vec<String> = std::env::args().collect();

    // List all input devices
    let input_devices: Vec<_> = host.input_devices().unwrap().collect();
    println!("Input devices:");
    for (i, d) in input_devices.iter().enumerate() {
        let name = d.name().unwrap_or_default();
        let config = d.default_input_config().map(|c| format!("{}Hz {}ch", c.sample_rate().0, c.channels())).unwrap_or("?".into());
        let marker = if name.to_lowercase().contains("cable") { " [CABLE]" } else { "" };
        println!("  {}: {} ({}){}", i, name, config, marker);
    }

    // Find CABLE Output
    let cable_idx = input_devices.iter().position(|d| {
        d.name().map(|n| n.to_lowercase().contains("cable")).unwrap_or(false)
    }).expect("CABLE Output not found");

    // Pick mic: from command line arg, or first non-CABLE device
    let mic_idx = if args.len() > 1 {
        args[1].parse::<usize>().expect("Usage: audio_test [mic_index]")
    } else {
        input_devices.iter().position(|d| {
            d.name().map(|n| !n.to_lowercase().contains("cable")).unwrap_or(false)
        }).expect("No non-CABLE mic found")
    };

    let mic = &input_devices[mic_idx];
    let cable = &input_devices[cable_idx];
    let mic_name = mic.name().unwrap_or_default();
    let cable_name = cable.name().unwrap_or_default();

    println!("\nRecording from:");
    println!("  Mic [{}]: {}", mic_idx, mic_name);
    println!("  Cable [{}]: {}", cable_idx, cable_name);

    let mic_config = mic.default_input_config().unwrap();
    let cable_config = cable.default_input_config().unwrap();

    println!("  Mic: {}Hz {}ch", mic_config.sample_rate().0, mic_config.channels());
    println!("  Cable: {}Hz {}ch", cable_config.sample_rate().0, cable_config.channels());

    let mic_channels = mic_config.channels() as usize;
    let cable_channels = cable_config.channels() as usize;
    let mic_rate = mic_config.sample_rate().0;
    let cable_rate = cable_config.sample_rate().0;

    let mic_samples = Arc::new(Mutex::new(Vec::<f32>::new()));
    let cable_samples = Arc::new(Mutex::new(Vec::<f32>::new()));
    let running = Arc::new(AtomicBool::new(true));

    let ms = mic_samples.clone();
    let r1 = running.clone();
    let mic_stream = mic.build_input_stream(
        &mic_config.into(),
        move |data: &[f32], _| {
            if !r1.load(Ordering::Relaxed) { return; }
            let mut s = ms.lock().unwrap();
            for chunk in data.chunks(mic_channels) {
                s.push(chunk[0]);
            }
        },
        |e| eprintln!("Mic error: {}", e),
        None,
    ).unwrap();

    let cs = cable_samples.clone();
    let r2 = running.clone();
    let cable_stream = cable.build_input_stream(
        &cable_config.into(),
        move |data: &[f32], _| {
            if !r2.load(Ordering::Relaxed) { return; }
            let mut s = cs.lock().unwrap();
            for chunk in data.chunks(cable_channels) {
                s.push(chunk[0]);
            }
        },
        |e| eprintln!("Cable error: {}", e),
        None,
    ).unwrap();

    mic_stream.play().unwrap();
    cable_stream.play().unwrap();

    println!("\nRecording for 5 seconds... Speak now!");
    std::thread::sleep(std::time::Duration::from_secs(5));

    running.store(false, Ordering::Relaxed);
    drop(mic_stream);
    drop(cable_stream);

    let mic_data = mic_samples.lock().unwrap();
    let cable_data = cable_samples.lock().unwrap();

    println!("Mic: {} samples ({:.2}s)", mic_data.len(), mic_data.len() as f64 / mic_rate as f64);
    println!("Cable: {} samples ({:.2}s)", cable_data.len(), cable_data.len() as f64 / cable_rate as f64);

    let save = |path: &str, data: &[f32], rate: u32| {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for &s in data {
            w.write_sample(s).unwrap();
        }
        w.finalize().unwrap();
    };

    save("tests/dual_mic.wav", &mic_data, mic_rate);
    save("tests/dual_cable.wav", &cable_data, cable_rate);

    let rms = |d: &[f32]| -> f32 { (d.iter().map(|s| s * s).sum::<f32>() / d.len() as f32).sqrt() };
    let m_rms = rms(&mic_data);
    let c_rms = rms(&cable_data);
    let m_peak = mic_data.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
    let c_peak = cable_data.iter().map(|s| s.abs()).fold(0.0f32, f32::max);

    println!("\nMic   RMS: {:.4} ({:.1} dBFS)  Peak: {:.4}", m_rms, 20.0 * m_rms.log10(), m_peak);
    println!("Cable RMS: {:.4} ({:.1} dBFS)  Peak: {:.4}", c_rms, 20.0 * c_rms.log10(), c_peak);
    println!("Cable/Mic ratio: {:.2}x", c_rms / m_rms);
    println!("\nSaved: tests/dual_mic.wav and tests/dual_cable.wav");
}
