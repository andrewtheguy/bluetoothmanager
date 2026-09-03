# bluetoothmanager

A terminal Bluetooth manager for Linux. It talks to BlueZ (`bluetoothd`) directly over D-Bus, so there is no `bluetoothctl` wrapping and no polling: the screen updates as the bus reports changes.

- Adapters and devices side by side, with connection, pairing, trust and signal state.
- Pair and connect in one keystroke. PIN, passkey and confirmation prompts open as dialogs; the manager registers itself as the default pairing agent while it runs.
- Scan, power, discoverable and pairable toggles per adapter.
- Trust, block, forget and rename devices; rename adapters.
- Vim-style and arrow-key navigation, `?` for the key reference.

## Install

Prebuilt binaries for `linux-amd64` and `linux-arm64` are attached to every release. They are built on Ubuntu 24.04 and link against its glibc, so they need a distribution of similar age or newer.

```sh
curl -fsSL https://raw.githubusercontent.com/andrewtheguy/bluetoothmanager/main/install.sh | sh
```

The script installs to `/usr/local/bin` when that is writable, otherwise to `~/.local/bin`. Override with `INSTALL_DIR`, or pin a release with `VERSION`:

```sh
INSTALL_DIR=~/bin VERSION=v0.0.1 sh install.sh
```

Or grab a binary yourself from the [releases page](https://github.com/andrewtheguy/bluetoothmanager/releases).

### From source

Needs a recent stable Rust toolchain (edition 2024).

```sh
cargo install --git https://github.com/andrewtheguy/bluetoothmanager
```

## Requirements

- Linux with BlueZ running (`systemctl status bluetooth`).
- Permission to talk to `org.bluez` on the system bus. Most distributions grant this to local logged-in users, or to members of the `bluetooth` group.

## Usage

```sh
bluetoothmanager
```

There are no options. `-h` prints a short usage note and `-V` prints the version.

| Key | Action |
| --- | --- |
| `↑ ↓` / `j k` | move within the focused pane |
| `tab` / `← →` | switch between adapters and devices |
| `g` / `G` | jump to the first or last row |
| `enter` | pair (if needed) and connect to the selected device |
| `d` | disconnect the selected device |
| `f` | forget the device: remove it and its pairing keys |
| `t` | trust / untrust: let the device reconnect on its own |
| `b` | block / unblock all connections from the device |
| `n` | rename the selected device or adapter |
| `s` / `r` | start or stop scanning for nearby devices |
| `w` | turn the Bluetooth radio on or off |
| `v` | make this computer visible to other devices |
| `p` | allow or refuse incoming pairing requests |
| `esc` | dismiss a message or close a dialog |
| `?` | show this key reference |
| `q` / `ctrl-c` | quit |

## Releasing

Bump `version` in `Cargo.toml`, merge to `main`, then run the **Release (Manual)** workflow from the Actions tab. It tags `v<version>`, builds both architectures and publishes the binaries to a GitHub release. Runs from a branch other than `main` are marked as pre-releases.

