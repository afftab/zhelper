# ZHelper

A lightweight TUI for managing ASUS ROG / TUF laptops on Linux. Control battery charge limits, CPU power profiles, GPU modes, and audio devices from a single keyboard-driven interface.

---

## Features

**Battery**
- Charge limit (20-100%) with presets and fine-grained adjustment
- Persistent across reboots via systemd service + udev rule
- Live stats: voltage, current, power draw, cycle count, health %

**CPU**
- Thermal profile switching (Silent / Balanced / Turbo) via ASUS WMI
- Governor control (performance / powersave)
- CPU boost toggle
- Energy Performance Preference (EPP) selection
- PPT power limit adjustment (sustained, fast, PL1, PL2)
- NVIDIA dynamic boost and temp target

**GPU**
- dGPU mode switching: Eco (dGPU off) / Standard (dGPU on) via ASUS WMI
- Live CPU/GPU temperature and fan RPM monitoring

**Audio**
- Output and input device management via PipeWire/PulseAudio
- Volume control, mute toggle, default device selection
- Port switching (speakers/headphones, internal mic/headset mic)

**System**
- CPU/GPU temperatures, memory usage, thermal zones, AC status

---

## Requirements

- Linux with ASUS `asus-nb-wmi` kernel module (5.4+, 5.9+ recommended)
- PipeWire or PulseAudio (for audio features)
- `pactl` installed (usually bundled with your desktop environment)

Verify battery charge control support:
```bash
ls /sys/class/power_supply/BAT*/charge_control_end_threshold
```

---

## Installation

### Build from source

```bash
git clone https://github.com/Ichihiroy/ghelper-for-linux
cd ghelper-for-linux
cargo build --release
sudo cp target/release/zhelper /usr/local/bin/
```

Then run from anywhere:
```bash
zhelper
```

---

## First-time Setup

Battery charge limit writes require root. Run the one-time setup to create a udev rule and systemd service:

```bash
sudo bash setup.sh 80
```

Or inside the app: navigate to the **battery** tab and press `s`.

After setup, charge limit changes apply without a password prompt and persist across reboots.

---

## Keybindings

| Key | Action |
|-----|--------|
| `Tab` | Switch sidebar/content (or output/input in audio tab) |
| `j` / `k` | Navigate items |
| `←` / `→` | Adjust values or navigate modes |
| `a` / `Enter` | Apply selected setting |
| `Space` | Toggle mute / boost / settings |
| `s` | Battery persistence setup |
| `d` | Set default audio device |
| `r` | Force refresh |
| `q` | Quit |

---

## Supported Hardware

Any ASUS laptop with the `asus-nb-wmi` kernel module, including ROG Zephyrus (G14/G15/G16/M16), ROG Flow, ROG Strix, TUF Gaming, and VivoBook series.

---

## License

MIT -- see [LICENSE](LICENSE)
