#![windows_subsystem = "windows"]

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use eframe::egui;
use std::io::BufReader;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([320.0, 300.0])
            .with_resizable(false)
            .with_title("Microboost"),
        ..Default::default()
    };

    eframe::run_native(
        "Microboost",
        options,
        Box::new(|_cc| Ok(Box::new(MicroboostApp::new()))),
    )
}

struct MicroboostApp {
    devices: Vec<String>,
    selected_device: usize,
    boost: u32,
    is_recording: bool,
    recording_start: Option<std::time::Instant>,
    last_recording: Option<PathBuf>,
    sample_rate: u32,
    status: String,

    host: cpal::Host,
    stream: Arc<Mutex<Option<cpal::Stream>>>,
    recording_active: Arc<Mutex<bool>>,
    samples: Arc<Mutex<Vec<f32>>>,
}

impl MicroboostApp {
    fn new() -> Self {
        let host = cpal::default_host();
        let mut devices = Vec::new();

        if let Ok(devs) = host.input_devices() {
            for d in devs {
                if let Ok(name) = d.name() {
                    devices.push(name);
                }
            }
        }

        Self {
            devices,
            selected_device: 0,
            boost: 100,
            is_recording: false,
            recording_start: None,
            last_recording: None,
            sample_rate: 44100,
            status: "Ready".to_string(),
            host,
            stream: Arc::new(Mutex::new(None)),
            recording_active: Arc::new(Mutex::new(false)),
            samples: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn start_recording(&mut self) {
        let device = self
            .host
            .input_devices()
            .ok()
            .and_then(|mut devs| devs.nth(self.selected_device))
            .or_else(|| self.host.default_input_device());

        if let Some(device) = device {
            if let Ok(supported_config) = device.default_input_config() {
                self.sample_rate = supported_config.sample_rate().0;

                let config: cpal::StreamConfig = supported_config.clone().into();
                *self.samples.lock().unwrap() = Vec::new();
                *self.recording_active.lock().unwrap() = true;

                let samples = self.samples.clone();
                let active = self.recording_active.clone();
                let gain = self.boost as f32 / 100.0;
                let input_channels = supported_config.channels() as usize;

                if let Ok(stream) = device.build_input_stream(
                    &config,
                    move |data: &[f32], _| {
                        if *active.lock().unwrap() {
                            let mut s = samples.lock().unwrap();
                            for chunk in data.chunks(input_channels) {
                                let mono = chunk.iter().sum::<f32>() / chunk.len() as f32;
                                s.push((mono * gain).clamp(-1.0, 1.0));
                            }
                        }
                    },
                    |e| eprintln!("Audio error: {}", e),
                    None,
                ) {
                    if stream.play().is_ok() {
                        *self.stream.lock().unwrap() = Some(stream);
                        self.is_recording = true;
                        self.recording_start = Some(std::time::Instant::now());
                        self.status = "Recording...".to_string();
                    }
                }
            }
        }
    }

    fn stop_recording(&mut self) {
        *self.recording_active.lock().unwrap() = false;
        *self.stream.lock().unwrap() = None;

        let samples = self.samples.lock().unwrap().clone();
        if samples.is_empty() {
            self.status = "No audio recorded".to_string();
            return;
        }

        let folder =
            PathBuf::from(std::env::var("APPDATA").unwrap_or(".".to_string())).join("Microboost");
        let _ = std::fs::create_dir_all(&folder);

        let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let path = folder.join(format!("test_{}.wav", timestamp));

        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: self.sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };

        if let Ok(mut writer) = hound::WavWriter::create(&path, spec) {
            for sample in &samples {
                let amp = (*sample * i16::MAX as f32) as i16;
                let _ = writer.write_sample(amp);
            }
            let _ = writer.finalize();

            let elapsed = self
                .recording_start
                .map(|s| s.elapsed().as_secs())
                .unwrap_or(0);
            self.last_recording = Some(path);
            self.status = format!("Saved: {}s", elapsed);
        }

        self.is_recording = false;
        self.recording_start = None;
    }

    fn play_recording(&mut self) {
        if let Some(ref path) = self.last_recording {
            let path = path.clone();
            self.status = "Playing...".to_string();

            let (_stream, handle) = match rodio::OutputStream::try_default() {
                Ok(pair) => pair,
                Err(_) => {
                    self.status = "Audio error".to_string();
                    return;
                }
            };

            let file = std::fs::File::open(&path).ok();
            if let Some(file) = file {
                let source = rodio::Decoder::new(BufReader::new(file));
                if let Ok(source) = source {
                    let sink = rodio::Sink::try_new(&handle).ok();
                    if let Some(sink) = sink {
                        sink.append(source);
                        sink.sleep_until_end();
                        self.status = "Ready".to_string();
                        return;
                    }
                }
            }
            self.status = "Playback failed".to_string();
        }
    }

    fn open_folder(&self) {
        let folder =
            PathBuf::from(std::env::var("APPDATA").unwrap_or(".".to_string())).join("Microboost");
        let _ = std::fs::create_dir_all(&folder);
        let _ = std::process::Command::new("explorer").arg(&folder).spawn();
    }
}

impl eframe::App for MicroboostApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.is_recording {
            ctx.request_repaint();
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(8.0);

            ui.label("Microphone");
            egui::ComboBox::from_id_salt("devices")
                .width(280.0)
                .selected_text(
                    self.devices
                        .get(self.selected_device)
                        .cloned()
                        .unwrap_or_default(),
                )
                .show_ui(ui, |ui| {
                    for (i, name) in self.devices.iter().enumerate() {
                        ui.selectable_value(&mut self.selected_device, i, name);
                    }
                });

            ui.add_space(12.0);

            let boost_steps = [20, 50, 100, 150, 200, 300, 500, 1000];

            ui.horizontal(|ui| {
                ui.label(format!("Boost: {}%", self.boost));
                ui.add(
                    egui::DragValue::new(&mut self.boost)
                        .range(0..=1000)
                        .suffix("%"),
                );
            });

            ui.add_space(8.0);

            ui.push_id("boost_slider", |ui| {
                ui.spacing_mut().slider_width = 280.0;
                let slider_resp = ui.add(
                    egui::Slider::new(&mut self.boost, 0..=1000)
                        .show_value(false)
                        .step_by(1.0)
                        .trailing_fill(true),
                );
                if slider_resp.changed() {
                    for &step in &boost_steps {
                        if (self.boost as i32 - step as i32).abs() < 15 {
                            self.boost = step;
                            break;
                        }
                    }
                }
            });

            ui.add_space(12.0);

            let btn_text = if self.is_recording {
                let elapsed = self
                    .recording_start
                    .map(|s| s.elapsed().as_secs() as u32)
                    .unwrap_or(0);
                format!("Stop ({:02}:{:02})", elapsed / 60, elapsed % 60)
            } else {
                "Record Test".to_string()
            };

            let btn = ui.add_sized([280.0, 40.0], egui::Button::new(&btn_text));
            if btn.clicked() {
                if self.is_recording {
                    self.stop_recording();
                } else {
                    self.start_recording();
                }
            }

            ui.add_space(8.0);

            ui.horizontal(|ui| {
                if ui
                    .add_enabled(self.last_recording.is_some(), egui::Button::new("Play"))
                    .on_disabled_hover_text("No recording yet")
                    .clicked()
                {
                    self.play_recording();
                }
                if ui.button("Folder").clicked() {
                    self.open_folder();
                }
            });

            ui.add_space(8.0);
            ui.label(&self.status);
        });
    }
}
