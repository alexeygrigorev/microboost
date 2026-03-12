#![windows_subsystem = "windows"]

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use eframe::egui;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// VB-CABLE auto-setup
mod vbcable {
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::{Arc, Mutex};

    fn app_dir() -> PathBuf {
        let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(base).join("Microboost")
    }

    /// Check if VB-CABLE is installed by looking for its devices
    pub fn is_installed() -> bool {
        let host = cpal::default_host();
        use cpal::traits::HostTrait;
        if let Ok(devs) = host.output_devices() {
            use cpal::traits::DeviceTrait;
            for d in devs {
                if let Ok(name) = d.name() {
                    if name.to_lowercase().contains("cable input") {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Download VB-CABLE zip
    pub fn download(status: &Arc<Mutex<String>>) -> Result<PathBuf, String> {
        let dir = app_dir();
        let _ = std::fs::create_dir_all(&dir);
        let zip_path = dir.join("VBCABLE_Driver_Pack.zip");

        // If already downloaded, skip
        if zip_path.exists() {
            let meta = std::fs::metadata(&zip_path).ok();
            if meta.map(|m| m.len() > 100_000).unwrap_or(false) {
                return Ok(zip_path);
            }
        }

        *status.lock().unwrap() = "Downloading VB-CABLE...".to_string();

        // Use PowerShell to download (available on all modern Windows)
        let result = Command::new("powershell")
            .args([
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                &format!(
                    "[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12; \
                     Invoke-WebRequest -Uri 'https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack43.zip' \
                     -OutFile '{}'",
                    zip_path.display()
                ),
            ])
            .output();

        match result {
            Ok(output) if output.status.success() && zip_path.exists() => Ok(zip_path),
            Ok(output) => {
                let err = String::from_utf8_lossy(&output.stderr);
                Err(format!("Download failed: {}", err))
            }
            Err(e) => Err(format!("Could not run PowerShell: {}", e)),
        }
    }

    /// Extract the zip and run the installer with admin privileges
    pub fn install(zip_path: &PathBuf, status: &Arc<Mutex<String>>) -> Result<(), String> {
        let dir = app_dir().join("vbcable");
        let _ = std::fs::create_dir_all(&dir);

        *status.lock().unwrap() = "Extracting VB-CABLE...".to_string();

        // Extract using PowerShell
        let extract = Command::new("powershell")
            .args([
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                &format!(
                    "Expand-Archive -Path '{}' -DestinationPath '{}' -Force",
                    zip_path.display(),
                    dir.display()
                ),
            ])
            .output();

        if extract.is_err() || !extract.as_ref().unwrap().status.success() {
            return Err("Failed to extract VB-CABLE".to_string());
        }

        // Find the 64-bit setup exe
        let setup_exe = dir.join("VBCABLE_Setup_x64.exe");
        if !setup_exe.exists() {
            return Err(format!(
                "Setup not found at {}. Check extraction.",
                setup_exe.display()
            ));
        }

        *status.lock().unwrap() =
            "Installing VB-CABLE (admin prompt)...".to_string();

        // Run installer elevated (will trigger UAC prompt)
        let install = Command::new("powershell")
            .args([
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                &format!(
                    "Start-Process -FilePath '{}' -ArgumentList '-i','-h' -Verb RunAs -Wait",
                    setup_exe.display()
                ),
            ])
            .output();

        match install {
            Ok(output) if output.status.success() => {
                // Give Windows a moment to register the device
                std::thread::sleep(std::time::Duration::from_secs(2));
                if is_installed() {
                    Ok(())
                } else {
                    // Device might need a restart of audio service
                    Err("Installed but device not yet visible. Try restarting the app.".to_string())
                }
            }
            Ok(output) => {
                let err = String::from_utf8_lossy(&output.stderr);
                Err(format!("Install failed: {}", err))
            }
            Err(e) => Err(format!("Could not launch installer: {}", e)),
        }
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([380.0, 440.0])
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

#[derive(PartialEq)]
enum SetupState {
    NotInstalled,
    Downloading,
    Failed(String),
    Ready,
}

struct MicroboostApp {
    host: cpal::Host,
    input_devices: Vec<String>,
    output_devices: Vec<String>,
    selected_input: usize,
    selected_output: usize,

    boost: u32,
    is_active: bool,
    status: Arc<Mutex<String>>,

    // VB-CABLE setup
    setup_state: SetupState,
    setup_thread: Option<std::thread::JoinHandle<Result<(), String>>>,

    // Audio pipeline
    input_stream: Arc<Mutex<Option<cpal::Stream>>>,
    output_stream: Arc<Mutex<Option<cpal::Stream>>>,
    ring_buffer: Arc<Mutex<Vec<f32>>>,
    ring_read: Arc<Mutex<usize>>,
    ring_write: Arc<Mutex<usize>>,
    pipeline_active: Arc<Mutex<bool>>,

    // Test recording
    is_recording: bool,
    recording_start: Option<std::time::Instant>,
    last_recording: Option<PathBuf>,
    sample_rate: u32,
    rec_stream: Arc<Mutex<Option<cpal::Stream>>>,
    rec_active: Arc<Mutex<bool>>,
    rec_samples: Arc<Mutex<Vec<f32>>>,
}

const RING_SIZE: usize = 48000 * 2;

impl MicroboostApp {
    fn new() -> Self {
        let host = cpal::default_host();
        let (input_devices, output_devices) = Self::enumerate_devices(&host);

        let cable_installed = vbcable::is_installed();
        let selected_output = Self::find_cable_output(&output_devices).unwrap_or(0);

        Self {
            host,
            input_devices,
            output_devices,
            selected_input: 0,
            selected_output,
            boost: 200,
            is_active: false,
            status: Arc::new(Mutex::new("Ready".to_string())),
            setup_state: if cable_installed {
                SetupState::Ready
            } else {
                SetupState::NotInstalled
            },
            setup_thread: None,
            input_stream: Arc::new(Mutex::new(None)),
            output_stream: Arc::new(Mutex::new(None)),
            ring_buffer: Arc::new(Mutex::new(vec![0.0; RING_SIZE])),
            ring_read: Arc::new(Mutex::new(0)),
            ring_write: Arc::new(Mutex::new(0)),
            pipeline_active: Arc::new(Mutex::new(false)),
            is_recording: false,
            recording_start: None,
            last_recording: None,
            sample_rate: 48000,
            rec_stream: Arc::new(Mutex::new(None)),
            rec_active: Arc::new(Mutex::new(false)),
            rec_samples: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn enumerate_devices(host: &cpal::Host) -> (Vec<String>, Vec<String>) {
        let inputs: Vec<String> = host
            .input_devices()
            .map(|devs| {
                devs.filter_map(|d| d.name().ok())
                    .filter(|name| {
                        let lower = name.to_lowercase();
                        !lower.contains("cable output") && !lower.contains("cable input")
                    })
                    .collect()
            })
            .unwrap_or_default();
        let outputs: Vec<String> = host
            .output_devices()
            .map(|devs| devs.filter_map(|d| d.name().ok()).collect())
            .unwrap_or_default();
        (inputs, outputs)
    }

    fn find_cable_output(devices: &[String]) -> Option<usize> {
        devices.iter().position(|name| {
            let lower = name.to_lowercase();
            lower.contains("cable input")
        })
    }

    fn refresh_devices(&mut self) {
        let (inputs, outputs) = Self::enumerate_devices(&self.host);
        self.input_devices = inputs;
        self.output_devices = outputs;
        if let Some(idx) = Self::find_cable_output(&self.output_devices) {
            self.selected_output = idx;
        }
    }

    fn start_vbcable_install(&mut self) {
        self.setup_state = SetupState::Downloading;
        let status = self.status.clone();

        self.setup_thread = Some(std::thread::spawn(move || {
            let zip = vbcable::download(&status)?;
            vbcable::install(&zip, &status)?;
            *status.lock().unwrap() = "VB-CABLE installed!".to_string();
            Ok(())
        }));
    }

    fn check_setup_thread(&mut self) {
        if let Some(handle) = &self.setup_thread {
            if handle.is_finished() {
                let handle = self.setup_thread.take().unwrap();
                match handle.join() {
                    Ok(Ok(())) => {
                        self.setup_state = SetupState::Ready;
                        self.refresh_devices();
                    }
                    Ok(Err(e)) => {
                        self.setup_state = SetupState::Failed(e);
                    }
                    Err(_) => {
                        self.setup_state =
                            SetupState::Failed("Setup thread panicked".to_string());
                    }
                }
            }
        }
    }

    fn start_pipeline(&mut self) {
        let input_device = self
            .host
            .input_devices()
            .ok()
            .and_then(|mut devs| devs.nth(self.selected_input));

        let output_device = self
            .host
            .output_devices()
            .ok()
            .and_then(|mut devs| devs.nth(self.selected_output));

        let (input_device, output_device) = match (input_device, output_device) {
            (Some(i), Some(o)) => (i, o),
            _ => {
                *self.status.lock().unwrap() = "Could not open audio devices".to_string();
                return;
            }
        };

        let in_config = match input_device.default_input_config() {
            Ok(c) => c,
            Err(e) => {
                *self.status.lock().unwrap() = format!("Input config error: {}", e);
                return;
            }
        };

        let out_supported = match output_device.default_output_config() {
            Ok(c) => c,
            Err(e) => {
                *self.status.lock().unwrap() = format!("Output config error: {}", e);
                return;
            }
        };

        let in_channels = in_config.channels() as usize;
        let out_channels = out_supported.channels() as usize;
        let in_sample_rate = in_config.sample_rate();
        let out_sample_rate = out_supported.sample_rate();

        let out_config = cpal::StreamConfig {
            channels: out_supported.channels(),
            sample_rate: out_sample_rate,
            buffer_size: cpal::BufferSize::Default,
        };

        // Simple sample rate ratio for conversion
        let rate_ratio = in_sample_rate.0 as f64 / out_sample_rate.0 as f64;

        // Reset ring buffer
        {
            let mut buf = self.ring_buffer.lock().unwrap();
            buf.iter_mut().for_each(|s| *s = 0.0);
            *self.ring_read.lock().unwrap() = 0;
            *self.ring_write.lock().unwrap() = 0;
        }

        *self.pipeline_active.lock().unwrap() = true;

        let ring_buf = self.ring_buffer.clone();
        let ring_w = self.ring_write.clone();
        let active = self.pipeline_active.clone();
        let gain = self.boost as f32 / 100.0;

        let in_stream_config: cpal::StreamConfig = in_config.into();
        let input_stream = input_device.build_input_stream(
            &in_stream_config,
            move |data: &[f32], _| {
                if !*active.lock().unwrap() {
                    return;
                }
                let mut buf = ring_buf.lock().unwrap();
                let mut w = ring_w.lock().unwrap();
                for chunk in data.chunks(in_channels) {
                    let mono = chunk.iter().sum::<f32>() / chunk.len() as f32;
                    let boosted = (mono * gain).clamp(-1.0, 1.0);
                    buf[*w % RING_SIZE] = boosted;
                    *w = (*w + 1) % (RING_SIZE * 2);
                }
            },
            |e| eprintln!("Input error: {}", e),
            None,
        );

        let ring_buf2 = self.ring_buffer.clone();
        let ring_r = self.ring_read.clone();
        let ring_w2 = self.ring_write.clone();
        let active2 = self.pipeline_active.clone();
        // Track fractional read position for sample rate conversion
        let frac_pos = Arc::new(Mutex::new(0.0f64));

        let output_stream = output_device.build_output_stream(
            &out_config,
            move |data: &mut [f32], _| {
                if !*active2.lock().unwrap() {
                    data.iter_mut().for_each(|s| *s = 0.0);
                    return;
                }
                let buf = ring_buf2.lock().unwrap();
                let mut r = ring_r.lock().unwrap();
                let w = *ring_w2.lock().unwrap();
                let mut frac = frac_pos.lock().unwrap();

                // Output is interleaved with out_channels per frame
                for frame in data.chunks_mut(out_channels) {
                    let sample = if *r != w {
                        let s = buf[*r % RING_SIZE];
                        // Advance read position by rate ratio
                        *frac += rate_ratio;
                        while *frac >= 1.0 {
                            *frac -= 1.0;
                            if *r != w {
                                *r = (*r + 1) % (RING_SIZE * 2);
                            }
                        }
                        s
                    } else {
                        0.0 // underrun
                    };
                    // Write same sample to all output channels
                    for ch in frame.iter_mut() {
                        *ch = sample;
                    }
                }
            },
            |e| eprintln!("Output error: {}", e),
            None,
        );

        match (input_stream, output_stream) {
            (Ok(is), Ok(os)) => {
                if is.play().is_ok() && os.play().is_ok() {
                    *self.input_stream.lock().unwrap() = Some(is);
                    *self.output_stream.lock().unwrap() = Some(os);
                    self.is_active = true;
                    let out_name = self
                        .output_devices
                        .get(self.selected_output)
                        .cloned()
                        .unwrap_or_default();
                    *self.status.lock().unwrap() = format!(
                        "Boosting {:.0}x -> {}",
                        self.boost as f32 / 100.0,
                        out_name
                    );
                } else {
                    *self.status.lock().unwrap() = "Failed to start audio streams".to_string();
                }
            }
            (Err(e), _) => {
                *self.status.lock().unwrap() = format!("Input stream error: {}", e);
            }
            (_, Err(e)) => {
                *self.status.lock().unwrap() = format!("Output stream error: {}", e);
            }
        }
    }

    fn stop_pipeline(&mut self) {
        *self.pipeline_active.lock().unwrap() = false;
        *self.input_stream.lock().unwrap() = None;
        *self.output_stream.lock().unwrap() = None;
        self.is_active = false;
        *self.status.lock().unwrap() = "Stopped".to_string();
    }

    fn update_gain(&mut self) {
        if self.is_active {
            self.stop_pipeline();
            self.start_pipeline();
        }
    }

    fn start_recording(&mut self) {
        *self.rec_samples.lock().unwrap() = Vec::new();
        *self.rec_active.lock().unwrap() = true;
        self.is_recording = true;
        self.recording_start = Some(std::time::Instant::now());

        if self.is_active {
            // Pipeline is running — tap into the ring buffer
            let ring_buf = self.ring_buffer.clone();
            let ring_w = self.ring_write.clone();
            let rec_samples = self.rec_samples.clone();
            let rec_active = self.rec_active.clone();
            self.sample_rate = 48000; // approximate, will be close enough

            std::thread::spawn(move || {
                let mut last_w = *ring_w.lock().unwrap();
                while *rec_active.lock().unwrap() {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    let buf = ring_buf.lock().unwrap();
                    let w = *ring_w.lock().unwrap();
                    let mut samples = rec_samples.lock().unwrap();
                    while last_w != w {
                        samples.push(buf[last_w % RING_SIZE]);
                        last_w = (last_w + 1) % (RING_SIZE * 2);
                    }
                }
            });

            *self.status.lock().unwrap() = "Recording from boost pipeline...".to_string();
        } else {
            // Pipeline not running — open mic directly
            let device = self
                .host
                .input_devices()
                .ok()
                .and_then(|mut devs| devs.nth(self.selected_input))
                .or_else(|| self.host.default_input_device());

            if let Some(device) = device {
                if let Ok(supported_config) = device.default_input_config() {
                    self.sample_rate = supported_config.sample_rate().0;
                    let config: cpal::StreamConfig = supported_config.clone().into();
                    let samples = self.rec_samples.clone();
                    let active = self.rec_active.clone();
                    let input_channels = supported_config.channels() as usize;
                    let gain = self.boost as f32 / 100.0;

                    if let Ok(stream) = device.build_input_stream(
                        &config,
                        move |data: &[f32], _| {
                            if *active.lock().unwrap() {
                                let mut s = samples.lock().unwrap();
                                for chunk in data.chunks(input_channels) {
                                    let mono =
                                        chunk.iter().sum::<f32>() / chunk.len() as f32;
                                    s.push((mono * gain).clamp(-1.0, 1.0));
                                }
                            }
                        },
                        |e| eprintln!("Audio error: {}", e),
                        None,
                    ) {
                        if stream.play().is_ok() {
                            *self.rec_stream.lock().unwrap() = Some(stream);
                        }
                    }
                }
            }
            *self.status.lock().unwrap() = "Recording test (with boost)...".to_string();
        }
    }

    fn stop_recording(&mut self) {
        *self.rec_active.lock().unwrap() = false;
        *self.rec_stream.lock().unwrap() = None;

        let samples = self.rec_samples.lock().unwrap().clone();
        if samples.is_empty() {
            *self.status.lock().unwrap() = "No audio recorded".to_string();
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
            *self.status.lock().unwrap() = format!("Saved: {}s", elapsed);
        }

        self.is_recording = false;
        self.recording_start = None;
    }

    fn play_recording(&mut self) {
        if let Some(ref path) = self.last_recording {
            let path = path.clone();
            let status = self.status.clone();
            let samples = self.rec_samples.clone();
            let sample_rate = self.sample_rate;
            *status.lock().unwrap() = "Playing...".to_string();

            let host = cpal::default_host();
            let speaker = host.default_output_device();

            let speaker = match speaker {
                Some(s) => s,
                None => {
                    *status.lock().unwrap() = "No speaker found".to_string();
                    return;
                }
            };

            let out_config = match speaker.default_output_config() {
                Ok(c) => c,
                Err(e) => {
                    *status.lock().unwrap() = format!("Speaker error: {}", e);
                    return;
                }
            };

            let out_channels = out_config.channels() as usize;
            let out_rate = out_config.sample_rate().0;
            let config: cpal::StreamConfig = out_config.into();

            let samples_data = samples.lock().unwrap().clone();
            if samples_data.is_empty() {
                // Try loading from file
                if let Ok(mut reader) = hound::WavReader::open(&path) {
                    let loaded: Vec<f32> = reader
                        .samples::<i16>()
                        .filter_map(|s| s.ok())
                        .map(|s| s as f32 / i16::MAX as f32)
                        .collect();
                    if loaded.is_empty() {
                        *status.lock().unwrap() = "Empty recording".to_string();
                        return;
                    }
                    *samples.lock().unwrap() = loaded;
                } else {
                    *status.lock().unwrap() = "Could not read file".to_string();
                    return;
                }
            }

            let play_samples = samples.lock().unwrap().clone();
            let play_pos = Arc::new(Mutex::new(0usize));
            let play_done = Arc::new(Mutex::new(false));
            let done_clone = play_done.clone();
            let rate_ratio = sample_rate as f64 / out_rate as f64;
            let frac = Arc::new(Mutex::new(0.0f64));

            let stream = speaker.build_output_stream(
                &config,
                move |data: &mut [f32], _| {
                    let mut pos = play_pos.lock().unwrap();
                    let mut f = frac.lock().unwrap();
                    for frame in data.chunks_mut(out_channels) {
                        if *pos < play_samples.len() {
                            let sample = play_samples[*pos];
                            for ch in frame.iter_mut() {
                                *ch = sample;
                            }
                            *f += rate_ratio;
                            while *f >= 1.0 {
                                *f -= 1.0;
                                *pos += 1;
                            }
                        } else {
                            for ch in frame.iter_mut() {
                                *ch = 0.0;
                            }
                            *done_clone.lock().unwrap() = true;
                        }
                    }
                },
                |e| eprintln!("Playback error: {}", e),
                None,
            );

            match stream {
                Ok(stream) => {
                    if stream.play().is_ok() {
                        std::thread::spawn(move || {
                            // Wait until playback finishes
                            loop {
                                std::thread::sleep(std::time::Duration::from_millis(50));
                                if *play_done.lock().unwrap() {
                                    break;
                                }
                            }
                            drop(stream);
                            *status.lock().unwrap() = "Ready".to_string();
                        });
                    } else {
                        *status.lock().unwrap() = "Failed to start playback".to_string();
                    }
                }
                Err(e) => {
                    *status.lock().unwrap() = format!("Playback error: {}", e);
                }
            }
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
        // Poll setup thread
        self.check_setup_thread();

        if self.is_recording
            || self.is_active
            || matches!(self.setup_state, SetupState::Downloading)
        {
            ctx.request_repaint();
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(8.0);

            match &self.setup_state {
                SetupState::NotInstalled => {
                    self.show_setup_screen(ui);
                }
                SetupState::Downloading => {
                    self.show_progress_screen(ui);
                }
                SetupState::Failed(err) => {
                    let err = err.clone();
                    self.show_failed_screen(ui, &err);
                }
                SetupState::Ready => {
                    self.show_main_screen(ui);
                }
            }
        });
    }
}

impl MicroboostApp {
    fn show_setup_screen(&mut self, ui: &mut egui::Ui) {
        ui.heading("Setup Required");
        ui.add_space(12.0);

        ui.label("Microboost needs VB-CABLE (free) to route boosted audio to other apps.");
        ui.add_space(8.0);
        ui.label("VB-CABLE creates a virtual microphone that apps like Discord, Teams, etc. can use.");
        ui.add_space(16.0);

        ui.label("What will happen:");
        ui.label("  1. Download VB-CABLE (~1 MB)");
        ui.label("  2. Install it (admin prompt)");
        ui.label("  3. A new \"CABLE Output\" mic appears in Windows");
        ui.add_space(16.0);

        let btn = ui.add_sized(
            [320.0, 40.0],
            egui::Button::new(
                egui::RichText::new("Install VB-CABLE")
                    .color(egui::Color32::WHITE)
                    .strong(),
            )
            .fill(egui::Color32::from_rgb(60, 120, 200)),
        );
        if btn.clicked() {
            self.start_vbcable_install();
        }

        ui.add_space(8.0);
        if ui.link("Already have a virtual cable? Skip setup").clicked() {
            self.setup_state = SetupState::Ready;
            self.refresh_devices();
        }
    }

    fn show_progress_screen(&self, ui: &mut egui::Ui) {
        ui.heading("Setting up VB-CABLE...");
        ui.add_space(20.0);
        ui.spinner();
        ui.add_space(12.0);
        let status = self.status.lock().unwrap().clone();
        ui.label(&status);
        ui.add_space(8.0);
        ui.label("An admin prompt may appear — please accept it.");
    }

    fn show_failed_screen(&mut self, ui: &mut egui::Ui, err: &str) {
        ui.heading("Setup Failed");
        ui.add_space(12.0);
        ui.colored_label(egui::Color32::from_rgb(220, 80, 80), err);
        ui.add_space(16.0);

        if ui.button("Retry").clicked() {
            self.start_vbcable_install();
        }
        ui.add_space(8.0);
        if ui.link("Skip — I'll set up a virtual cable manually").clicked() {
            self.setup_state = SetupState::Ready;
            self.refresh_devices();
        }
    }

    fn show_main_screen(&mut self, ui: &mut egui::Ui) {
        ui.heading("Microphone Boost");
        ui.add_space(4.0);

        // Input device
        ui.label("Microphone");
        let prev_input = self.selected_input;
        egui::ComboBox::from_id_salt("input_device")
            .width(340.0)
            .selected_text(
                self.input_devices
                    .get(self.selected_input)
                    .cloned()
                    .unwrap_or("No input devices".to_string()),
            )
            .show_ui(ui, |ui| {
                for (i, name) in self.input_devices.iter().enumerate() {
                    ui.selectable_value(&mut self.selected_input, i, name);
                }
            });

        if self.is_active && prev_input != self.selected_input {
            self.stop_pipeline();
            self.start_pipeline();
        }

        ui.add_space(8.0);

        // Boost slider
        let boost_steps = [100, 200, 300, 500, 1000];
        ui.horizontal(|ui| {
            ui.label(format!(
                "Boost: {}% ({:.1}x)",
                self.boost,
                self.boost as f32 / 100.0
            ));
        });

        ui.add_space(4.0);

        ui.push_id("boost_slider", |ui| {
            ui.spacing_mut().slider_width = 340.0;
            let resp = ui.add(
                egui::Slider::new(&mut self.boost, 100..=1000)
                    .show_value(false)
                    .step_by(10.0)
                    .trailing_fill(true),
            );
            if resp.changed() {
                for &step in &boost_steps {
                    if (self.boost as i32 - step as i32).abs() < 20 {
                        self.boost = step;
                        break;
                    }
                }
            }
            if resp.drag_stopped() && self.is_active {
                self.update_gain();
            }
        });

        ui.add_space(4.0);

        ui.horizontal(|ui| {
            for &preset in &boost_steps {
                let label = format!("{:.0}x", preset as f32 / 100.0);
                if ui
                    .selectable_label(self.boost == preset, &label)
                    .clicked()
                {
                    self.boost = preset;
                    if self.is_active {
                        self.update_gain();
                    }
                }
            }
        });

        ui.add_space(8.0);

        // Start/Stop
        let btn_text = if self.is_active {
            "Stop Boost"
        } else {
            "Start Boost"
        };
        let btn_color = if self.is_active {
            egui::Color32::from_rgb(200, 60, 60)
        } else {
            egui::Color32::from_rgb(60, 160, 60)
        };
        let btn = ui.add_sized(
            [340.0, 36.0],
            egui::Button::new(
                egui::RichText::new(btn_text)
                    .color(egui::Color32::WHITE)
                    .strong(),
            )
            .fill(btn_color),
        );
        if btn.clicked() {
            if self.is_active {
                self.stop_pipeline();
            } else {
                self.start_pipeline();
            }
        }

        if self.is_active {
            ui.add_space(4.0);
            let cable_name = self
                .output_devices
                .get(self.selected_output)
                .map(|n| n.replace("CABLE Input", "CABLE Output"))
                .unwrap_or("CABLE Output".to_string());
            ui.colored_label(
                egui::Color32::from_rgb(100, 200, 100),
                format!("In Discord/Teams/etc, select \"{}\" as your mic", cable_name),
            );
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(4.0);

        // Test recording
        ui.horizontal(|ui| {
            let rec_text = if self.is_recording {
                let elapsed = self
                    .recording_start
                    .map(|s| s.elapsed().as_secs() as u32)
                    .unwrap_or(0);
                format!("Stop ({:02}:{:02})", elapsed / 60, elapsed % 60)
            } else {
                "Record Test".to_string()
            };

            if ui.button(&rec_text).clicked() {
                if self.is_recording {
                    self.stop_recording();
                } else {
                    self.start_recording();
                }
            }

            if ui
                .add_enabled(self.last_recording.is_some(), egui::Button::new("Play"))
                .clicked()
            {
                self.play_recording();
            }

            if ui.button("Folder").clicked() {
                self.open_folder();
            }

            if ui.button("Refresh Devices").clicked() {
                self.refresh_devices();
            }
        });

        ui.add_space(4.0);
        let status = self.status.lock().unwrap().clone();
        ui.label(&status);
    }
}
