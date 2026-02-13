# Microboost

A simple Windows microphone booster app with a native UI.

## Features

- **Microphone Selection** - Choose from all available input devices
- **Boost Slider** - Amplify your mic from 0% to 1000%
  - Sticky points at 20%, 50%, 100%, 150%, 200%, 300%, 500%, 1000%
  - Or type any custom value
- **Record Test** - Record a test clip to check your levels
- **Playback** - Play back your recording instantly
- **Native UI** - Built with egui, no web technologies

## Screenshots

<img width="407" height="428" alt="image" src="https://github.com/user-attachments/assets/3e0e66fb-932b-4c47-939a-5fe433ec9ca7" />

## Installation

Download the latest release from the [Releases](https://github.com/alexeygrigorev/microboost/releases) page.

Or build from source:

```bash
git clone https://github.com/alexeygrigorev/microboost.git
cd microboost
cargo build --release
```

The executable will be at `target/release/microboost.exe`.

## Usage

1. Select your microphone from the dropdown
2. Adjust the boost slider (100% = no change, 200% = 2x louder, etc.)
3. Click "Record Test" to record a sample
4. Click "Play" to hear how you sound
5. Click "Folder" to open the recordings folder

Recordings are saved to `%APPDATA%\Microboost\`.

## Development

### Requirements

- Rust (1.70+)
- Cargo

### Build

```bash
make build
```

### Run

```bash
make open
```

### Other commands

```bash
make run      # Build and run
make kill     # Kill running instance
make clean    # Clean build artifacts
make folder   # Open recordings folder
```

## Tech Stack

- [egui](https://github.com/emilk/egui) - Native GUI
- [cpal](https://github.com/RustAudio/cpal) - Audio capture
- [hound](https://github.com/ruuda/hound) - WAV encoding
- [rodio](https://github.com/RustAudio/rodio) - Audio playback

## License

MIT
