#![windows_subsystem = "windows"]

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use eframe::egui;
use microboost::{noise_gate, SpscRing, RING_SIZE};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

/// Device profiles — saved per microphone name
mod profiles {
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn profiles_path() -> PathBuf {
        let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(base).join("Microboost").join("profiles.json")
    }

    #[derive(Serialize, Deserialize, Clone, Debug)]
    pub struct Profile {
        pub boost: u32,
        #[serde(default)]
        pub noise_floor_rms: Option<f32>,
        #[serde(default)]
        pub noise_gate_enabled: bool,
    }

    /// Map from device name -> Profile
    pub type ProfileMap = HashMap<String, Profile>;

    pub fn load() -> ProfileMap {
        let path = profiles_path();
        if let Ok(data) = std::fs::read_to_string(&path) {
            serde_json::from_str(&data).unwrap_or_default()
        } else {
            HashMap::new()
        }
    }

    pub fn save(map: &ProfileMap) {
        let path = profiles_path();
        let _ = std::fs::create_dir_all(path.parent().unwrap());
        if let Ok(json) = serde_json::to_string_pretty(map) {
            let _ = std::fs::write(&path, json);
        }
    }

    fn settings_path() -> PathBuf {
        let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(base).join("Microboost").join("settings.json")
    }

    #[derive(Serialize, Deserialize, Default)]
    pub struct Settings {
        pub last_input_device: Option<String>,
    }

    pub fn load_settings() -> Settings {
        let path = settings_path();
        if let Ok(data) = std::fs::read_to_string(&path) {
            serde_json::from_str(&data).unwrap_or_default()
        } else {
            Settings::default()
        }
    }

    pub fn save_settings(settings: &Settings) {
        let path = settings_path();
        let _ = std::fs::create_dir_all(path.parent().unwrap());
        if let Ok(json) = serde_json::to_string_pretty(settings) {
            let _ = std::fs::write(&path, json);
        }
    }
}

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
            .with_inner_size([420.0, 740.0])
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

#[derive(PartialEq, Clone)]
enum CalibrationPhase {
    Idle,
    Listening,
    Done {
        boost_pct: u32,
        raw_db: f32,      // Your voice level in dBFS
        boosted_db: f32,   // After boost, in dBFS
        target_db: f32,    // YouTube target in dBFS
    },
}

const CALIBRATION_PHRASES: &[&str] = &[
    "Hello, testing one two three. This is my normal speaking voice.",
    "The quick brown fox jumps over the lazy dog near the river bank.",
    "I'm recording a video and want my audio to sound clear and loud.",
];

/// YouTube recommended voice target: ~-16 dBFS RMS (0.16 linear)
const TARGET_RMS: f32 = 0.16;


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
    ring_buffer: Arc<SpscRing>,
    pipeline_active: Arc<Mutex<bool>>,
    live_gain: Arc<Mutex<f32>>,       // Shared gain: updated without restarting pipeline
    live_input_rms: Arc<Mutex<f32>>,  // Raw input level for visualizer
    live_output_rms: Arc<Mutex<f32>>, // Boosted output level for visualizer
    input_history: Vec<f32>,   // Rolling waveform history (RMS values)
    output_history: Vec<f32>,
    vis_frame: u32,            // Frame counter for slowing down visualizer
    vis_accum_in: f32,         // Accumulated input RMS across frames
    vis_accum_out: f32,        // Accumulated output RMS across frames

    // Test recording
    is_recording: bool,
    recording_start: Option<std::time::Instant>,
    last_recording: Option<PathBuf>,
    sample_rate: u32,
    rec_stream: Arc<Mutex<Option<cpal::Stream>>>,
    rec_active: Arc<Mutex<bool>>,
    rec_samples: Arc<Mutex<Vec<f32>>>,

    // Auto-calibration
    cal_phase: CalibrationPhase,
    cal_start: Option<std::time::Instant>,
    cal_stream: Arc<Mutex<Option<cpal::Stream>>>,
    cal_active: Arc<Mutex<bool>>,
    cal_samples: Arc<Mutex<Vec<f32>>>,
    cal_rms_live: Arc<Mutex<f32>>,
    cal_phrase_idx: usize,

    // Profiles
    device_profiles: HashMap<String, profiles::Profile>,

    // Auto-start
    first_frame: bool,

    // Noise gate
    noise_gate: Arc<Mutex<noise_gate::NoiseGate>>,
    ng_cal_state: Arc<Mutex<noise_gate::CalibrationState>>,
    ng_calibrating: bool,
    ng_cal_start: Option<std::time::Instant>,
    ng_cal_stream: Arc<Mutex<Option<cpal::Stream>>>,

    // Hot-plug detection
    last_device_check: std::time::Instant,

}


impl MicroboostApp {
    fn new() -> Self {
        let host = cpal::default_host();
        let (input_devices, output_devices) = Self::enumerate_devices(&host);

        let cable_installed = vbcable::is_installed();
        let selected_output = Self::find_cable_output(&output_devices).unwrap_or(0);

        let device_profiles = profiles::load();
        let settings = profiles::load_settings();

        // Select input device: 1) last used, 2) Windows default, 3) first
        let selected_input = settings
            .last_input_device
            .as_ref()
            .and_then(|saved| input_devices.iter().position(|d| d == saved))
            .or_else(|| {
                // Try to match Windows default input device
                host.default_input_device()
                    .and_then(|d| d.name().ok())
                    .and_then(|default_name| {
                        input_devices.iter().position(|d| d == &default_name)
                    })
            })
            .unwrap_or(0);

        // Load saved boost for the selected device
        let current_profile = input_devices
            .get(selected_input)
            .and_then(|name| device_profiles.get(name));
        let boost = current_profile.map(|p| p.boost).unwrap_or(200);

        // Restore noise gate from profile
        let noise_gate = noise_gate::NoiseGate::new();
        let ng = {
            let mut ng = noise_gate;
            if let Some(profile) = current_profile {
                if let Some(floor) = profile.noise_floor_rms {
                    let headroom = ng.headroom;
                    ng.restore(floor, profile.noise_gate_enabled, headroom);
                }
            }
            ng
        };

        Self {
            host,
            input_devices,
            output_devices,
            selected_input,
            selected_output,
            boost,
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
            ring_buffer: Arc::new(SpscRing::new(RING_SIZE)),
            pipeline_active: Arc::new(Mutex::new(false)),
            live_gain: Arc::new(Mutex::new(1.0)),
            live_input_rms: Arc::new(Mutex::new(0.0)),
            live_output_rms: Arc::new(Mutex::new(0.0)),
            input_history: vec![0.0; 120],
            output_history: vec![0.0; 120],
            vis_frame: 0,
            vis_accum_in: 0.0,
            vis_accum_out: 0.0,
            is_recording: false,
            recording_start: None,
            last_recording: None,
            sample_rate: 48000,
            rec_stream: Arc::new(Mutex::new(None)),
            rec_active: Arc::new(Mutex::new(false)),
            rec_samples: Arc::new(Mutex::new(Vec::new())),

            cal_phase: CalibrationPhase::Idle,
            cal_start: None,
            cal_stream: Arc::new(Mutex::new(None)),
            cal_active: Arc::new(Mutex::new(false)),
            cal_samples: Arc::new(Mutex::new(Vec::new())),
            cal_rms_live: Arc::new(Mutex::new(0.0)),
            cal_phrase_idx: 0,

            device_profiles,

            first_frame: true,

            noise_gate: Arc::new(Mutex::new(ng)),
            ng_cal_state: noise_gate::new_calibration_state(),
            ng_calibrating: false,
            ng_cal_start: None,
            ng_cal_stream: Arc::new(Mutex::new(None)),

            last_device_check: std::time::Instant::now(),
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

    /// Find an input device by name (avoids index mismatch when CABLE is filtered from UI)
    fn find_input_device_by_name(host: &cpal::Host, name: &str) -> Option<cpal::Device> {
        host.input_devices().ok()?.find(|d| d.name().ok().as_deref() == Some(name))
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

    /// Periodically check for device changes (hot-plug).
    /// If the current device disappeared, switch to the Windows default (or first available).
    /// If a new default device appeared that wasn't there before, switch to it.
    fn check_device_changes(&mut self) {
        if self.last_device_check.elapsed().as_secs() < 2 {
            return;
        }
        self.last_device_check = std::time::Instant::now();

        let old_devices = self.input_devices.clone();
        let current_name = self.input_devices.get(self.selected_input).cloned();

        self.refresh_devices();

        // Check if device list actually changed
        if self.input_devices == old_devices {
            return;
        }

        // Get Windows default device name
        let default_name = self
            .host
            .default_input_device()
            .and_then(|d| d.name().ok());

        // If our current device is still present, keep it
        if let Some(ref name) = current_name {
            if let Some(pos) = self.input_devices.iter().position(|d| d == name) {
                self.selected_input = pos;
                return;
            }
        }

        // Current device disappeared — switch to default or first available
        let new_idx = default_name
            .as_ref()
            .and_then(|def| self.input_devices.iter().position(|d| d == def))
            .unwrap_or(0);

        if new_idx != self.selected_input || current_name.is_none() {
            self.selected_input = new_idx;
            self.load_profile_for_device();

            // Restart pipeline with new device
            let was_active = self.is_active;
            self.kill_pipeline();
            if !self.input_devices.is_empty() {
                self.start_pipeline();
                if !was_active {
                    self.stop_pipeline();
                }
            }

            let dev_name = self
                .input_devices
                .get(self.selected_input)
                .cloned()
                .unwrap_or("none".to_string());
            *self.status.lock().unwrap() =
                format!("Device changed — switched to {}", dev_name);
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
        self.save_current_profile();

        // If pipeline is already running, just update gain
        if *self.pipeline_active.lock().unwrap() {
            *self.live_gain.lock().unwrap() = self.boost as f32 / 100.0;
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
            return;
        }

        let input_device = self
            .input_devices
            .get(self.selected_input)
            .and_then(|name| Self::find_input_device_by_name(&self.host, name));

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

        let rate_ratio = in_sample_rate.0 as f64 / out_sample_rate.0 as f64;

        eprintln!(
            "Pipeline: in={}Hz out={}Hz ratio={:.4} in_ch={} out_ch={}",
            in_sample_rate.0, out_sample_rate.0, rate_ratio, in_channels, out_channels
        );

        // Reset ring buffer
        self.ring_buffer.reset();

        *self.pipeline_active.lock().unwrap() = true;

        let ring = self.ring_buffer.clone();
        let active = self.pipeline_active.clone();
        let gain_shared = self.live_gain.clone();
        let input_rms = self.live_input_rms.clone();
        let output_rms = self.live_output_rms.clone();
        let ng = self.noise_gate.clone();

        // Set gain to current boost level
        *self.live_gain.lock().unwrap() = self.boost as f32 / 100.0;

        let in_stream_config: cpal::StreamConfig = in_config.into();
        let input_stream = input_device.build_input_stream(
            &in_stream_config,
            move |data: &[f32], _| {
                if !*active.lock().unwrap() {
                    return;
                }
                let gain = *gain_shared.lock().unwrap();
                let mut gate = ng.lock().unwrap();
                let mut sum_raw = 0.0f32;
                let mut sum_out = 0.0f32;
                let mut count = 0usize;
                for chunk in data.chunks(in_channels) {
                    let mono = chunk[0];
                    let boosted = (mono * gain).clamp(-1.0, 1.0);
                    // Apply noise gate after boost
                    let gated = gate.process(boosted);
                    ring.push(gated);
                    sum_raw += mono * mono;
                    sum_out += gated * gated;
                    count += 1;
                }
                if count > 0 {
                    let raw = (sum_raw / count as f32).sqrt();
                    let out = (sum_out / count as f32).sqrt();
                    let mut ir = input_rms.lock().unwrap();
                    let mut or = output_rms.lock().unwrap();
                    *ir = *ir * 0.8 + raw * 0.2;
                    *or = *or * 0.8 + out * 0.2;
                }
            },
            |e| eprintln!("Input error: {}", e),
            None,
        );

        let ring2 = self.ring_buffer.clone();
        let active2 = self.pipeline_active.clone();

        let output_stream = output_device.build_output_stream(
            &out_config,
            move |data: &mut [f32], _| {
                if !*active2.lock().unwrap() {
                    data.iter_mut().for_each(|s| *s = 0.0);
                    return;
                }

                for frame in data.chunks_mut(out_channels) {
                    let sample = if ring2.available() > 0 {
                        let s = ring2.peek(0);
                        ring2.advance(1);
                        s
                    } else {
                        0.0
                    };
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
                        "Boosting {:.1}x -> {} ({}Hz {}ch -> {}Hz {}ch)",
                        self.boost as f32 / 100.0,
                        out_name,
                        in_sample_rate.0,
                        in_channels,
                        out_sample_rate.0,
                        out_channels,
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
        // Don't kill the streams — just set gain to 1x passthrough
        *self.live_gain.lock().unwrap() = 1.0;
        self.is_active = false;
        *self.status.lock().unwrap() = "Passthrough (1x)".to_string();
    }

    fn kill_pipeline(&mut self) {
        // Actually stop the audio streams (used on device switch)
        *self.pipeline_active.lock().unwrap() = false;
        *self.input_stream.lock().unwrap() = None;
        *self.output_stream.lock().unwrap() = None;
        self.is_active = false;
        *self.status.lock().unwrap() = "Stopped".to_string();
    }

    fn save_current_profile(&mut self) {
        if let Some(name) = self.input_devices.get(self.selected_input) {
            let gate = self.noise_gate.lock().unwrap();
            let noise_floor_rms = if gate.is_calibrated() {
                Some(gate.noise_floor_rms())
            } else {
                None
            };
            let noise_gate_enabled = gate.enabled;
            drop(gate);

            self.device_profiles.insert(
                name.clone(),
                profiles::Profile {
                    boost: self.boost,
                    noise_floor_rms,
                    noise_gate_enabled,
                },
            );
            profiles::save(&self.device_profiles);
            profiles::save_settings(&profiles::Settings {
                last_input_device: Some(name.clone()),
            });
        }
    }

    fn load_profile_for_device(&mut self) {
        if let Some(name) = self.input_devices.get(self.selected_input) {
            if let Some(profile) = self.device_profiles.get(name) {
                self.boost = profile.boost;
                let mut gate = self.noise_gate.lock().unwrap();
                if let Some(floor) = profile.noise_floor_rms {
                    let headroom = gate.headroom;
                    gate.restore(floor, profile.noise_gate_enabled, headroom);
                } else {
                    gate.enabled = false;
                }
            }
        }
    }

    fn update_gain(&mut self) {
        if self.is_active {
            // Just update the shared gain — no need to restart pipeline
            *self.live_gain.lock().unwrap() = self.boost as f32 / 100.0;
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
        }
    }

    fn start_calibration(&mut self) {
        // Keep pipeline running — calibration opens a second input stream for raw samples

        *self.cal_samples.lock().unwrap() = Vec::new();
        *self.cal_active.lock().unwrap() = true;
        *self.cal_rms_live.lock().unwrap() = 0.0;
        self.cal_phase = CalibrationPhase::Listening;
        self.cal_start = Some(std::time::Instant::now());
        // Pick a random phrase
        self.cal_phrase_idx =
            (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_millis() as usize)
                % CALIBRATION_PHRASES.len();

        let device = self
            .input_devices
            .get(self.selected_input)
            .and_then(|name| Self::find_input_device_by_name(&self.host, name))
            .or_else(|| self.host.default_input_device());

        if let Some(device) = device {
            if let Ok(supported_config) = device.default_input_config() {
                let config: cpal::StreamConfig = supported_config.clone().into();
                let input_channels = supported_config.channels() as usize;
                let samples = self.cal_samples.clone();
                let active = self.cal_active.clone();
                let rms_live = self.cal_rms_live.clone();
                let sample_rate = supported_config.sample_rate().0 as usize;

                if let Ok(stream) = device.build_input_stream(
                    &config,
                    move |data: &[f32], _| {
                        if !*active.lock().unwrap() {
                            return;
                        }
                        let mut s = samples.lock().unwrap();
                        for chunk in data.chunks(input_channels) {
                            let mono = chunk[0];
                            s.push(mono); // Raw, no boost
                        }
                        // Update live RMS over last ~0.3s
                        let window = sample_rate / 3;
                        if s.len() > window {
                            let recent = &s[s.len() - window..];
                            let sum_sq: f32 =
                                recent.iter().map(|x| x * x).sum();
                            let rms = (sum_sq / recent.len() as f32).sqrt();
                            *rms_live.lock().unwrap() = rms;
                        }
                    },
                    |e| eprintln!("Calibration error: {}", e),
                    None,
                ) {
                    if stream.play().is_ok() {
                        *self.cal_stream.lock().unwrap() = Some(stream);
                    }
                }
            }
        }
        *self.status.lock().unwrap() = "Calibrating — speak now...".to_string();
    }

    fn finish_calibration(&mut self) {
        *self.cal_active.lock().unwrap() = false;
        *self.cal_stream.lock().unwrap() = None;

        let raw_rms = {
            let samples = self.cal_samples.lock().unwrap();
            if samples.len() < 4800 {
                self.cal_phase = CalibrationPhase::Idle;
                *self.status.lock().unwrap() = "Calibration failed — not enough audio".to_string();
                return;
            }

            // Compute RMS of entire capture, ignoring silence (gate at -50 dBFS)
            let silence_gate = 0.003_f32; // ~-50 dBFS
            let voiced: Vec<f32> = samples
                .iter()
                .copied()
                .filter(|s| s.abs() > silence_gate)
                .collect();

            if voiced.len() < 2400 {
                self.cal_phase = CalibrationPhase::Idle;
                *self.status.lock().unwrap() =
                    "Calibration failed — couldn't detect speech. Try speaking louder.".to_string();
                return;
            }

            let sum_sq: f32 = voiced.iter().map(|x| x * x).sum();
            (sum_sq / voiced.len() as f32).sqrt()
        };

        // Calculate needed boost
        let needed = TARGET_RMS / raw_rms;
        let boost_pct = (needed * 100.0).round() as u32;
        let boost_pct = boost_pct.clamp(10, 500);

        let raw_db = 20.0 * raw_rms.log10();
        let boosted_rms = (raw_rms * boost_pct as f32 / 100.0).min(1.0);
        let boosted_db = 20.0 * boosted_rms.log10();
        let target_db = 20.0 * TARGET_RMS.log10();

        self.cal_phase = CalibrationPhase::Done {
            boost_pct,
            raw_db,
            boosted_db,
            target_db,
        };
        // Pipeline kept running — no need to restart
        *self.status.lock().unwrap() = format!(
            "Voice: {:.1} dB -> Boosted: {:.1} dB (target: {:.1} dB)",
            raw_db, boosted_db, target_db
        );
    }

    fn cancel_calibration(&mut self) {
        *self.cal_active.lock().unwrap() = false;
        *self.cal_stream.lock().unwrap() = None;
        self.cal_phase = CalibrationPhase::Idle;
        *self.status.lock().unwrap() = "Calibration cancelled".to_string();
    }

    fn start_noise_calibration(&mut self) {
        // Keep pipeline running — calibration opens a second input stream for raw samples

        {
            let mut cal = self.ng_cal_state.lock().unwrap();
            cal.active = true;
            cal.samples.clear();
        }
        self.ng_calibrating = true;
        self.ng_cal_start = Some(std::time::Instant::now());

        let device = self
            .input_devices
            .get(self.selected_input)
            .and_then(|name| Self::find_input_device_by_name(&self.host, name))
            .or_else(|| self.host.default_input_device());

        if let Some(device) = device {
            if let Ok(supported_config) = device.default_input_config() {
                let config: cpal::StreamConfig = supported_config.clone().into();
                let input_channels = supported_config.channels() as usize;
                let cal_state = self.ng_cal_state.clone();

                if let Ok(stream) = device.build_input_stream(
                    &config,
                    move |data: &[f32], _| {
                        let mut cal = cal_state.lock().unwrap();
                        if !cal.active {
                            return;
                        }
                        for chunk in data.chunks(input_channels) {
                            let mono = chunk[0];
                            cal.samples.push(mono);
                        }
                    },
                    |e| eprintln!("Noise cal error: {}", e),
                    None,
                ) {
                    if stream.play().is_ok() {
                        *self.ng_cal_stream.lock().unwrap() = Some(stream);
                    }
                }
            }
        }
        *self.status.lock().unwrap() = "Noise calibration — stay SILENT...".to_string();
    }

    fn finish_noise_calibration(&mut self) {
        {
            let mut cal = self.ng_cal_state.lock().unwrap();
            cal.active = false;
        }
        *self.ng_cal_stream.lock().unwrap() = None;
        self.ng_calibrating = false;
        self.ng_cal_start = None;

        let samples = {
            let cal = self.ng_cal_state.lock().unwrap();
            cal.samples.clone()
        };

        let mut gate = self.noise_gate.lock().unwrap();
        match gate.finish_calibration(&samples) {
            Ok(db) => {
                *self.status.lock().unwrap() = format!(
                    "Noise floor: {:.1} dBFS | Gate threshold: {:.1} dBFS | Gate ON",
                    db,
                    gate.threshold_db()
                );
            }
            Err(e) => {
                *self.status.lock().unwrap() = format!("Noise calibration failed: {}", e);
            }
        }
        drop(gate);

        // Save noise gate to profile (pipeline keeps running)
        self.save_current_profile();
    }

    fn cancel_noise_calibration(&mut self) {
        {
            let mut cal = self.ng_cal_state.lock().unwrap();
            cal.active = false;
        }
        *self.ng_cal_stream.lock().unwrap() = None;
        self.ng_calibrating = false;
        self.ng_cal_start = None;
        *self.status.lock().unwrap() = "Noise calibration cancelled".to_string();
    }

    fn start_recording(&mut self) {
        *self.rec_samples.lock().unwrap() = Vec::new();
        *self.rec_active.lock().unwrap() = true;
        self.is_recording = true;
        self.recording_start = Some(std::time::Instant::now());

        if self.is_active {
            // Pipeline is running — tap into the ring buffer
            let ring = self.ring_buffer.clone();
            let rec_samples = self.rec_samples.clone();
            let rec_active = self.rec_active.clone();
            self.sample_rate = 48000; // approximate, will be close enough

            std::thread::spawn(move || {
                let mut last_w = ring.write.load(Ordering::Acquire);
                while *rec_active.lock().unwrap() {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    let w = ring.write.load(Ordering::Acquire);
                    let mut samples = rec_samples.lock().unwrap();
                    while last_w != w {
                        samples.push(ring.read_at(last_w));
                        last_w = (last_w + 1) % (RING_SIZE * 2);
                    }
                }
            });

            *self.status.lock().unwrap() = "Recording from boost pipeline...".to_string();
        } else {
            // Pipeline not running — open mic directly
            let device = self
                .input_devices
                .get(self.selected_input)
                .and_then(|name| Self::find_input_device_by_name(&self.host, name))
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
                                        chunk[0];
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

        // Auto-start pipeline on first frame
        if self.first_frame && self.setup_state == SetupState::Ready {
            self.first_frame = false;
            self.start_pipeline();
        }

        // Periodic device hot-plug detection
        if self.setup_state == SetupState::Ready
            && !self.ng_calibrating
            && self.cal_phase == CalibrationPhase::Idle
        {
            self.check_device_changes();
        }

        // Auto-finish calibration after 5 seconds
        if self.cal_phase == CalibrationPhase::Listening {
            if let Some(start) = self.cal_start {
                if start.elapsed().as_secs() >= 5 {
                    self.finish_calibration();
                }
            }
        }

        // Auto-finish noise calibration after 3 seconds
        if self.ng_calibrating {
            if let Some(start) = self.ng_cal_start {
                if start.elapsed().as_secs() >= 3 {
                    self.finish_noise_calibration();
                }
            }
        }

        let pipeline_running = *self.pipeline_active.lock().unwrap();
        if self.is_recording
            || self.is_active
            || pipeline_running
            || self.ng_calibrating
            || self.cal_phase == CalibrationPhase::Listening
            || matches!(self.setup_state, SetupState::Downloading)
        {
            ctx.request_repaint();
        } else if self.setup_state == SetupState::Ready {
            // Repaint every 2s for device hot-plug detection even when idle
            ctx.request_repaint_after(std::time::Duration::from_secs(2));
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
        ui.horizontal(|ui| {
            ui.label("Microphone");
            if let Some(name) = self.input_devices.get(self.selected_input) {
                if self.device_profiles.contains_key(name) {
                    ui.label(
                        egui::RichText::new("(saved profile)")
                            .small()
                            .color(egui::Color32::from_rgb(120, 180, 120)),
                    );
                }
            }
        });
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

        if prev_input != self.selected_input {
            // Save boost for old device, load for new one
            if let Some(old_name) = self.input_devices.get(prev_input) {
                let gate = self.noise_gate.lock().unwrap();
                let noise_floor_rms = if gate.is_calibrated() {
                    Some(gate.noise_floor_rms())
                } else {
                    None
                };
                let noise_gate_enabled = gate.enabled;
                drop(gate);
                self.device_profiles.insert(
                    old_name.clone(),
                    profiles::Profile {
                        boost: self.boost,
                        noise_floor_rms,
                        noise_gate_enabled,
                    },
                );
            }
            self.load_profile_for_device();
            profiles::save(&self.device_profiles);

            // Restart pipeline with new device
            let was_active = self.is_active;
            self.kill_pipeline();
            self.start_pipeline();
            if !was_active {
                // Was in passthrough mode — go back to passthrough
                self.stop_pipeline();
            }
        }

        ui.add_space(8.0);

        // Boost slider (capped at 500%, manual entry for higher)
        let boost_presets = [10, 25, 50, 100, 150, 200, 300, 400, 500];
        let prev_boost = self.boost;
        ui.horizontal(|ui| {
            ui.label("Boost:");
            let drag = ui.add(
                egui::DragValue::new(&mut self.boost)
                    .range(10..=1000)
                    .speed(10)
                    .suffix("%"),
            );
            ui.label(format!("({:.1}x)", self.boost as f32 / 100.0));
            if drag.changed() {
                // Round to nearest 10
                self.boost = ((self.boost + 5) / 10) * 10;
                self.boost = self.boost.clamp(10, 1000);
            }
            if drag.lost_focus() && self.is_active && self.boost != prev_boost {
                self.update_gain();
            }
        });

        ui.add_space(4.0);

        // Slider caps at 500%
        let slider_val = self.boost.clamp(10, 500);
        let mut slider_boost = slider_val;
        ui.push_id("boost_slider", |ui| {
            ui.spacing_mut().slider_width = 340.0;
            let resp = ui.add(
                egui::Slider::new(&mut slider_boost, 10..=500)
                    .show_value(false)
                    .step_by(10.0)
                    .trailing_fill(true),
            );
            if resp.changed() {
                self.boost = slider_boost;
                for &step in &boost_presets {
                    if (self.boost as i32 - step as i32).abs() < 15 {
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
            for &preset in &boost_presets {
                let label = format!("{:.1}x", preset as f32 / 100.0);
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

        ui.add_space(4.0);

        // Auto-calibration section
        match &self.cal_phase {
            CalibrationPhase::Idle => {
                let cal_btn = ui.add_sized(
                    [340.0, 28.0],
                    egui::Button::new(
                        egui::RichText::new("Auto-Calibrate (detect my voice level)")
                            .color(egui::Color32::WHITE),
                    )
                    .fill(egui::Color32::from_rgb(100, 80, 180)),
                );
                if cal_btn.clicked() {
                    self.start_calibration();
                }
            }
            CalibrationPhase::Listening => {
                let elapsed = self
                    .cal_start
                    .map(|s| s.elapsed().as_secs_f32())
                    .unwrap_or(0.0);
                let remaining = (5.0 - elapsed).max(0.0);

                ui.group(|ui| {
                    ui.set_width(340.0);
                    ui.colored_label(
                        egui::Color32::from_rgb(255, 200, 60),
                        "Speak now at your normal volume:",
                    );
                    ui.add_space(2.0);
                    ui.label(
                        egui::RichText::new(
                            CALIBRATION_PHRASES[self.cal_phrase_idx],
                        )
                        .italics(),
                    );
                    ui.add_space(4.0);

                    // Live level meter
                    let live_rms = *self.cal_rms_live.lock().unwrap();
                    let db = if live_rms > 0.0001 {
                        20.0 * live_rms.log10()
                    } else {
                        -80.0
                    };
                    // Map -60dB..0dB to 0..1
                    let level = ((db + 60.0) / 60.0).clamp(0.0, 1.0);

                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(340.0, 12.0),
                        egui::Sense::hover(),
                    );
                    let painter = ui.painter();
                    painter.rect_filled(
                        rect,
                        3.0,
                        egui::Color32::from_rgb(40, 40, 40),
                    );
                    let bar_color = if level > 0.85 {
                        egui::Color32::from_rgb(220, 60, 60)
                    } else if level > 0.6 {
                        egui::Color32::from_rgb(60, 200, 60)
                    } else {
                        egui::Color32::from_rgb(60, 140, 200)
                    };
                    let bar_rect = egui::Rect::from_min_size(
                        rect.min,
                        egui::vec2(rect.width() * level, rect.height()),
                    );
                    painter.rect_filled(bar_rect, 3.0, bar_color);

                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        ui.label(format!("{:.1}s remaining", remaining));
                        if ui.small_button("Cancel").clicked() {
                            self.cancel_calibration();
                        }
                    });
                });
            }
            CalibrationPhase::Done { boost_pct, raw_db, boosted_db, target_db } => {
                let boost_val = *boost_pct;
                let raw_db = *raw_db;
                let boosted_db = *boosted_db;
                let target_db = *target_db;
                ui.group(|ui| {
                    ui.set_width(340.0);
                    ui.colored_label(
                        egui::Color32::from_rgb(100, 200, 100),
                        format!(
                            "Recommended boost: {:.1}x ({}%)",
                            boost_val as f32 / 100.0,
                            boost_val
                        ),
                    );

                    // Visual level comparison
                    ui.add_space(4.0);
                    let bar_width = 300.0;
                    // Map dBFS: -60..0 -> 0..1
                    let raw_frac = ((raw_db + 60.0) / 60.0).clamp(0.0, 1.0);
                    let boosted_frac = ((boosted_db + 60.0) / 60.0).clamp(0.0, 1.0);
                    let target_frac = ((target_db + 60.0) / 60.0).clamp(0.0, 1.0);

                    // Your voice level
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Your mic ").small());
                        let (rect, _) = ui.allocate_exact_size(
                            egui::vec2(bar_width, 10.0), egui::Sense::hover(),
                        );
                        let p = ui.painter();
                        p.rect_filled(rect, 2.0, egui::Color32::from_rgb(40, 40, 40));
                        p.rect_filled(
                            egui::Rect::from_min_size(rect.min, egui::vec2(rect.width() * raw_frac, 10.0)),
                            2.0, egui::Color32::from_rgb(80, 130, 200),
                        );
                        // Target marker line
                        let tx = rect.min.x + rect.width() * target_frac;
                        p.line_segment(
                            [egui::pos2(tx, rect.min.y), egui::pos2(tx, rect.max.y)],
                            egui::Stroke::new(2.0, egui::Color32::from_rgb(255, 200, 60)),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(format!("           {:.1} dBFS", raw_db)).small()
                            .color(egui::Color32::from_rgb(80, 130, 200)));
                    });

                    // Boosted level
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Boosted  ").small());
                        let (rect, _) = ui.allocate_exact_size(
                            egui::vec2(bar_width, 10.0), egui::Sense::hover(),
                        );
                        let p = ui.painter();
                        p.rect_filled(rect, 2.0, egui::Color32::from_rgb(40, 40, 40));
                        p.rect_filled(
                            egui::Rect::from_min_size(rect.min, egui::vec2(rect.width() * boosted_frac, 10.0)),
                            2.0, egui::Color32::from_rgb(60, 200, 60),
                        );
                        let tx = rect.min.x + rect.width() * target_frac;
                        p.line_segment(
                            [egui::pos2(tx, rect.min.y), egui::pos2(tx, rect.max.y)],
                            egui::Stroke::new(2.0, egui::Color32::from_rgb(255, 200, 60)),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(format!("           {:.1} dBFS", boosted_db)).small()
                            .color(egui::Color32::from_rgb(60, 200, 60)));
                        ui.label(egui::RichText::new(format!("  | target: {:.1} dBFS", target_db)).small()
                            .color(egui::Color32::from_rgb(255, 200, 60)));
                    });

                    if boost_val >= 500 {
                        ui.label(
                            egui::RichText::new(
                                "Capped at 5x. Use manual entry above for higher values.",
                            )
                            .small()
                            .color(egui::Color32::from_rgb(180, 180, 120)),
                        );
                    }
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        if ui.button("Accept").clicked() {
                            self.boost = boost_val;
                            if *self.pipeline_active.lock().unwrap() {
                                self.update_gain();
                            } else {
                                self.start_pipeline();
                            }
                            self.save_current_profile();
                            self.cal_phase = CalibrationPhase::Idle;
                        }
                        if ui.button("Re-calibrate").clicked() {
                            self.start_calibration();
                        }
                        if ui.button("Dismiss").clicked() {
                            self.cal_phase = CalibrationPhase::Idle;
                        }
                    });
                });
            }
        }

        ui.add_space(4.0);

        // Noise gate
        if self.ng_calibrating {
            let elapsed = self.ng_cal_start
                .map(|s| s.elapsed().as_secs_f32())
                .unwrap_or(0.0);
            let remaining = (3.0 - elapsed).max(0.0);
            ui.group(|ui| {
                ui.set_width(340.0);
                ui.colored_label(
                    egui::Color32::from_rgb(255, 200, 60),
                    "Stay SILENT — capturing background noise...",
                );
                ui.label(format!("{:.1}s remaining", remaining));
                if ui.small_button("Cancel").clicked() {
                    self.cancel_noise_calibration();
                }
            });
        } else {
            let mut ng_changed = false;
            ui.horizontal(|ui| {
                let mut gate = self.noise_gate.lock().unwrap();
                let is_calibrated = gate.is_calibrated();
                let floor_db = gate.noise_floor_db();

                let prev_enabled = gate.enabled;
                let mut enabled = gate.enabled;
                ui.checkbox(&mut enabled, "Noise Gate");
                gate.enabled = enabled && is_calibrated;
                if gate.enabled != prev_enabled {
                    ng_changed = true;
                }
                drop(gate);

                if is_calibrated {
                    ui.label(
                        egui::RichText::new(format!("floor: {:.0} dB", floor_db))
                            .small()
                            .color(if enabled {
                                egui::Color32::from_rgb(100, 200, 100)
                            } else {
                                egui::Color32::GRAY
                            }),
                    );
                } else {
                    ui.label(
                        egui::RichText::new("(not calibrated)")
                            .small()
                            .color(egui::Color32::GRAY),
                    );
                }
                if ui.small_button("Calibrate").clicked() {
                    self.start_noise_calibration();
                }
            });
            if ng_changed {
                self.save_current_profile();
            }
        }

        ui.add_space(4.0);

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

        // Live audio waveform visualizer — both waves overlaid
        if *self.pipeline_active.lock().unwrap() {
            ui.add_space(4.0);
            let in_rms = *self.live_input_rms.lock().unwrap();
            let out_rms = *self.live_output_rms.lock().unwrap();

            // Push to rolling history every 4 frames (4x slower scroll)
            self.vis_accum_in += in_rms;
            self.vis_accum_out += out_rms;
            self.vis_frame += 1;
            if self.vis_frame >= 4 {
                self.input_history.push(self.vis_accum_in / 4.0);
                self.output_history.push(self.vis_accum_out / 4.0);
                self.vis_frame = 0;
                self.vis_accum_in = 0.0;
                self.vis_accum_out = 0.0;
            }
            let max_points = 150;
            if self.input_history.len() > max_points {
                self.input_history.drain(0..self.input_history.len() - max_points);
            }
            if self.output_history.len() > max_points {
                self.output_history.drain(0..self.output_history.len() - max_points);
            }

            let wave_w = 370.0;
            let wave_h = 60.0;
            let in_color = egui::Color32::from_rgb(60, 140, 220);
            let out_color = egui::Color32::from_rgb(80, 220, 80);

            // Legend
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("---").small().color(in_color));
                ui.label(egui::RichText::new("Input").small());
                ui.label(egui::RichText::new("---").small().color(out_color));
                ui.label(egui::RichText::new("Output (boosted)").small());
            });

            let (rect, _) = ui.allocate_exact_size(
                egui::vec2(wave_w, wave_h),
                egui::Sense::hover(),
            );
            let p = ui.painter();
            p.rect_filled(rect, 3.0, egui::Color32::from_rgb(20, 20, 25));

            let rms_to_frac = |rms: f32| -> f32 {
                if rms > 0.0001 {
                    ((20.0 * rms.log10() + 60.0) / 60.0).clamp(0.0, 1.0)
                } else {
                    0.0
                }
            };

            let make_points = |history: &[f32]| -> Vec<egui::Pos2> {
                let n = history.len();
                if n < 2 {
                    return vec![];
                }
                history
                    .iter()
                    .enumerate()
                    .map(|(i, &rms)| {
                        let x = rect.min.x + (i as f32 / (n - 1) as f32) * rect.width();
                        let frac = rms_to_frac(rms);
                        let y = rect.max.y - frac * rect.height();
                        egui::pos2(x, y)
                    })
                    .collect()
            };

            // Draw filled area between input and output (shows the boost difference)
            let in_pts = make_points(&self.input_history);
            let out_pts = make_points(&self.output_history);

            if in_pts.len() >= 2 && out_pts.len() >= 2 {
                // Fill between the two curves to show boost amount
                let n = in_pts.len().min(out_pts.len());
                let fill_color = egui::Color32::from_rgba_premultiplied(40, 180, 40, 25);
                // Build polygon strips column by column; skip when curves cross
                // to avoid bowtie artifacts from convex_polygon on non-convex quads
                for i in 0..n - 1 {
                    let in_above_l = in_pts[i].y <= out_pts[i].y;
                    let in_above_r = in_pts[i + 1].y <= out_pts[i + 1].y;
                    if in_above_l != in_above_r {
                        // Curves cross in this segment — skip fill to avoid artifacts
                        continue;
                    }
                    let quad = vec![
                        in_pts[i],
                        in_pts[i + 1],
                        out_pts[i + 1],
                        out_pts[i],
                    ];
                    p.add(egui::Shape::convex_polygon(
                        quad,
                        fill_color,
                        egui::Stroke::NONE,
                    ));
                }

                // Draw input line (thinner, behind)
                p.add(egui::Shape::line(
                    in_pts,
                    egui::Stroke::new(1.5, in_color),
                ));
                // Draw output line (thicker, on top)
                p.add(egui::Shape::line(
                    out_pts,
                    egui::Stroke::new(2.0, out_color),
                ));
            }

            // dB scale markers
            for &db in &[-40.0_f32, -20.0, -10.0] {
                let frac = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
                let y = rect.max.y - frac * rect.height();
                p.line_segment(
                    [egui::pos2(rect.min.x, y), egui::pos2(rect.max.x, y)],
                    egui::Stroke::new(0.5, egui::Color32::from_rgb(50, 50, 55)),
                );
                p.text(
                    egui::pos2(rect.max.x - 22.0, y - 6.0),
                    egui::Align2::LEFT_TOP,
                    format!("{}dB", db as i32),
                    egui::FontId::new(8.0, egui::FontFamily::Monospace),
                    egui::Color32::from_rgb(80, 80, 90),
                );
            }
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(4.0);

        // Profiles
        ui.collapsing("Saved Profiles", |ui| {
            let current_device = self.input_devices.get(self.selected_input).cloned();
            let mut to_delete: Option<String> = None;
            let mut to_load: Option<(String, u32)> = None;

            if self.device_profiles.is_empty() {
                ui.label(
                    egui::RichText::new("No saved profiles yet. Calibrate or start boost to save one.")
                        .small()
                        .color(egui::Color32::GRAY),
                );
            } else {
                let mut names: Vec<String> = self.device_profiles.keys().cloned().collect();
                names.sort();
                for name in &names {
                    let profile = &self.device_profiles[name];
                    let is_current = current_device.as_deref() == Some(name.as_str());
                    ui.horizontal(|ui| {
                        // Shorten long device names
                        let short_name = if name.len() > 30 {
                            format!("{}...", &name[..27])
                        } else {
                            name.clone()
                        };
                        let label = format!(
                            "{} — {:.1}x",
                            short_name,
                            profile.boost as f32 / 100.0
                        );
                        if is_current {
                            ui.label(
                                egui::RichText::new(&label)
                                    .small()
                                    .strong()
                                    .color(egui::Color32::from_rgb(100, 200, 100)),
                            );
                        } else if ui
                            .link(egui::RichText::new(&label).small())
                            .on_hover_text("Click to load this profile's boost level")
                            .clicked()
                        {
                            to_load = Some((name.clone(), profile.boost));
                        }
                        if ui.small_button("x").on_hover_text("Delete profile").clicked() {
                            to_delete = Some(name.clone());
                        }
                    });
                }
            }

            // Apply deferred actions
            if let Some(name) = to_delete {
                self.device_profiles.remove(&name);
                profiles::save(&self.device_profiles);
            }
            if let Some((_name, boost)) = to_load {
                self.boost = boost;
                if self.is_active {
                    self.update_gain();
                }
            }

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.small_button("Save current").on_hover_text("Save boost for current mic").clicked() {
                    self.save_current_profile();
                }
            });
        });

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

        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(format!("v{} ({})", env!("CARGO_PKG_VERSION"), env!("BUILD_TIMESTAMP")))
                .small()
                .color(egui::Color32::from_rgb(90, 90, 100)),
        );
    }
}
