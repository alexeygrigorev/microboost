# Microboost

A Windows microphone booster that amplifies your mic for other apps (Discord, Teams, etc.) using a real-time audio pipeline through VB-CABLE.

## How it works

Microboost captures audio from your real microphone, applies software gain (up to 10x), and routes the boosted audio to a virtual audio cable. Other apps then use the virtual cable as their microphone input, hearing the amplified audio.

On first launch, the app will offer to download and install VB-CABLE (free) automatically.

## Features

- Real-time microphone boost from 1x to 10x (100% to 1000%)
- **Auto-calibration**: detects your voice level and sets the boost to YouTube-recommended loudness (~-16 dBFS)
- Automatic VB-CABLE setup on first run
- Slider (up to 3x) with manual entry for higher values
- Test recording and playback to verify your levels
- Native UI built with egui

## Installation

Download the latest release from the [Releases](https://github.com/alexeygrigorev/microboost/releases) page.

Or build from source:

```bash
git clone https://github.com/alexeygrigorev/microboost.git
cd microboost
make build
```

The executable will be at `target/x86_64-pc-windows-msvc/release/microboost.exe`.

> **Note:** The build requires the MSVC target (`x86_64-pc-windows-msvc`). The Makefile handles this automatically.

## Usage

1. Launch Microboost. If VB-CABLE is not installed, click "Install VB-CABLE" and accept the admin prompt.
2. Select your microphone from the dropdown.
3. Click "Auto-Calibrate" to detect your voice level, or manually set the boost.
4. Click "Start Boost" (or "Accept & Start" after calibration).
5. In your other app (Discord, Teams, etc.), select "CABLE Output" as the microphone input.

Use "Record Test" and "Play" to verify the boost sounds right before going live.

Recordings are saved to `%APPDATA%\Microboost\`.

## Requirements

- Windows 10 or later
- VB-CABLE (installed automatically on first launch, or get it from https://vb-audio.com/Cable/)

## Development

### Build

```bash
make build      # Build release (MSVC target)
make run        # Build and run
make open       # Open the built executable
make kill       # Kill running instance
make clean      # Clean build artifacts
make folder     # Open recordings folder
make rebuild    # Kill, rebuild, then run: make open
```

## Tech Stack

- [egui](https://github.com/emilk/egui) - Native GUI
- [cpal](https://github.com/RustAudio/cpal) - Audio capture and playback
- [hound](https://github.com/ruuda/hound) - WAV encoding/decoding
- [VB-CABLE](https://vb-audio.com/Cable/) - Virtual audio cable driver

## License

MIT
