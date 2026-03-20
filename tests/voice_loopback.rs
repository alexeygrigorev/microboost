/// Send a real voice recording through VB-CABLE and record what comes back.
/// Compare the original vs looped-back signal.

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

#[test]
fn voice_through_cable() {
    let fixture = "tests/fixtures/test_voice.wav";
    if !std::path::Path::new(fixture).exists() {
        eprintln!("Skipping: {} not found (run audio_test/e2e_test first)", fixture);
        return;
    }
    let host = cpal::default_host();
    let has_cable = host.output_devices().unwrap().any(|d| d.name().map(|n| n.to_lowercase().contains("cable")).unwrap_or(false));
    if !has_cable {
        eprintln!("Skipping: VB-CABLE not installed");
        return;
    }
    std::fs::create_dir_all("tests/.tmp").ok();
    let (voice, voice_rate) = load_wav(fixture);
    eprintln!("Loaded voice: {} samples, {}Hz, {:.2}s",
        voice.len(), voice_rate, voice.len() as f64 / voice_rate as f64);

    let cable_out = find_device(&host, "CABLE Input", false)
        .expect("CABLE Input (playback) not found");
    let cable_in = find_device(&host, "CABLE Output", true)
        .expect("CABLE Output (recording) not found");

    let out_config = cable_out.default_output_config().unwrap();
    let in_config = cable_in.default_input_config().unwrap();
    let out_rate = out_config.sample_rate().0;
    let out_channels = out_config.channels() as usize;
    let in_channels = in_config.channels() as usize;

    eprintln!("CABLE out: {}Hz {}ch, CABLE in: {}Hz {}ch",
        out_rate, out_channels, in_config.sample_rate().0, in_channels);

    let recorded = Arc::new(Mutex::new(Vec::<f32>::new()));
    let rec_clone = recorded.clone();
    let write_pos = Arc::new(Mutex::new(0usize));
    let signal = voice.clone();

    // Start recording
    let in_stream_config: cpal::StreamConfig = in_config.into();
    let rec_stream = cable_in.build_input_stream(
        &in_stream_config,
        move |data: &[f32], _| {
            let mut rec = rec_clone.lock().unwrap();
            for chunk in data.chunks(in_channels) {
                rec.push(chunk[0]); // left channel
            }
        },
        |e| eprintln!("Record error: {}", e),
        None,
    ).unwrap();
    rec_stream.play().unwrap();

    std::thread::sleep(std::time::Duration::from_millis(200));

    // Start playback
    let out_stream_config: cpal::StreamConfig = out_config.into();
    let play_stream = cable_out.build_output_stream(
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

    // Wait for playback to finish + margin
    let duration_ms = (voice.len() as u64 * 1000 / voice_rate as u64) + 500;
    std::thread::sleep(std::time::Duration::from_millis(duration_ms));

    drop(play_stream);
    std::thread::sleep(std::time::Duration::from_millis(300));
    drop(rec_stream);

    let rec = recorded.lock().unwrap();
    eprintln!("Recorded {} samples ({:.2}s)", rec.len(), rec.len() as f64 / out_rate as f64);

    // Write original and looped-back to WAV files
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: out_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };

    let mut w1 = hound::WavWriter::create("tests/.tmp/voice_original.wav", spec).unwrap();
    for &s in voice.iter() {
        w1.write_sample(s).unwrap();
    }
    w1.finalize().unwrap();

    let mut w2 = hound::WavWriter::create("tests/.tmp/voice_through_cable.wav", spec).unwrap();
    for &s in rec.iter() {
        w2.write_sample(s).unwrap();
    }
    w2.finalize().unwrap();

    eprintln!("\nWrote:");
    eprintln!("  tests/.tmp/voice_original.wav");
    eprintln!("  tests/.tmp/voice_through_cable.wav");
    eprintln!("Listen to both and compare!");
}
