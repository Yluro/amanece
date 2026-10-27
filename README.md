# Amanece

Terminal UI for the ALBA BL13-XALOC/BL6-XAIRA autoprocessing results: browse your beamtimes over SFTP,
compare the statistics of every solve (xia2chem, ednaproc, autoPROC, xia2), download the HKL
files you pick into the folder you launched from, and run any commands (SHELXT, XPREP,...) on them.

Read-only, gentle and safe by design: one SFTP connection, rate-limited requests, and credentials are never stored. 
See [DESIGN.md](DESIGN.md).

## Installation

### Prebuilt binaries

Every tagged version is published under [Releases](https://github.com/Yluro/albawatch/releases)
with one archive per platform:

| Platform | Archive |
|---|---|
| Windows 10 / 11 (x64) | `amanece-<version>-x86_64-pc-windows-msvc.zip` |
| macOS, Intel | `amanece-<version>-x86_64-apple-darwin.tar.gz` |
| macOS, Apple silicon | `amanece-<version>-aarch64-apple-darwin.tar.gz` |
| Linux (x64, glibc) | `amanece-<version>-x86_64-unknown-linux-gnu.tar.gz` |

Unpack it and put `amanece` (`amanece.exe`) somewhere on your `PATH`. Each archive has a
`.sha256` file next to it.

**Requisites:**

- **Windows**: Windows 10 version 1809 or newer (the console panel needs ConPTY). The C/C++ runtime
  is linked into the `.exe` (static CRT, see [.cargo/config.toml](.cargo/config.toml)), so the
  *Microsoft Visual C++ Redistributable* is **not** needed. If you build your own copy without that
  setting, it is: install the 2015–2022 x64 package from <https://aka.ms/vs/17/release/vc_redist.x64.exe>
  or with `winget install Microsoft.VCRedist.2015+.x64`, otherwise the program fails to start with
  "VCRUNTIME140.dll was not found".
- **macOS**: Binaries downloaded with a browser carry a quarantine flag; clear it once with
  `xattr -d com.apple.quarantine amanece`.
- **Linux**: glibc 2.35 or newer (Ubuntu 22.04 / Debian 12 or newer; the binaries are built on Ubuntu 22.04).
  The C and C++ runtimes (`libc`, `libgcc_s`) are on every normal install; `xdg-open` is used to open files.

### Build from source

You need [Rust](https://rustup.rs) (stable, 1.85 or newer, the crate uses edition 2024) and a C/C++
toolchain, because some dependencies (`ring`) compile C code:

- **Windows**: *Visual Studio Build Tools* (or Visual Studio) with the **Desktop development with C++**
  workload: the MSVC compiler and linker plus a Windows 10/11 SDK. This is the same toolchain
  that rustup's `x86_64-pc-windows-msvc` target asks for. The repo's `.cargo/config.toml` makes the
  build use the static CRT, so the result runs without the redistributable.
  `winget install Microsoft.VisualStudio.2022.BuildTools` gets the installer.
- **macOS**: Xcode Command Line Tools, `xcode-select --install`.
- **Linux**: a C compiler and linker, e.g. `sudo apt install build-essential` (Debian / Ubuntu)
  or `sudo dnf groupinstall "Development Tools"` (Fedora).

## Build and run

```bash
cargo install --path .        # puts `amanece` on your PATH (~/.cargo/bin)
cd <folder where the data should go>
amanece                    # or: amanece --user u2025xxxxxxx
```

Each panel shows its keys on its bottom border; `?` lists them all. In short:

- **Local** (left): browse folders, `N` makes a new folder, `Supr` moves the selected file or folder
  to the Recycle Bin (asks first). Downloads land in `<sample>/<program>/` inside the folder open
  here, like the old notebook: `JSG285b/xia2chem_r3/shelxt.hkl`, `JSG285b/autoproc_noanom_r3/…`,
  `JSG285b/frames_r3/…`.
- **ALBA**: beamtimes like folders, from XALOC (BL13) and XAIRA (BL06) together, newest first;
  `Enter` opens one (the others are hidden), `Backspace` goes back.
  A green ◆ next to a sample: it's already downloaded (from this beamtime) into the Local folder;
  yellow ◆: only a local folder with the same name exists.
- **Stats**: every solve with its statistics, ★ on the best one. `Space` marks, `d` downloads,
  `F` downloads raw frames (asks first). Frames download a few files at a time (`parallel_downloads`
  in the config, default 3) with a progress bar above the status bar; more frame datasets wait their
  turn. HKL/log downloads (`d`) jump the queue and start right away, even while frames are
  downloading. If the connection drops, it reconnects and continues where each file stopped.
- **Console**: a terminal (Windows ConPTY). `:` to type. Commands run in the Local folder and a
  bare program name gets the selected file: with `XDS_ASCII.HKL` selected, `xprep` runs
  `xprep XDS_ASCII.HKL`. Interactive console programs work: while SADABS (or any program) runs,
  what you type plus Enter is its answer; Ctrl-C interrupts, twice kills. XPREP and other Windows
  GUI programs open in their own window.
- **Mouse**: click a panel or row, double-click to open, click a Stats checkbox to mark, wheel to
  scroll, click the status badges (poll / sound / logs) to toggle. Shift+drag still selects text
  in Windows Terminal.

`Tab` cycles panels, `p` cycles polling (manual → 10 min → 2 min → 30 s → 5 s; the fast rates only
re-list the open beamtime, sample rescans stay at most once a minute), `n` toggles the new-sample
sound, `q` quits (asks first).

## Files it writes locally

- `.alba_source.json` in each download folder – which server file each download came from
- `<launch folder>/.amanece/cache.json` – listings and stats already fetched
- `<launch folder>/.amanece/amanece.log` – console log (useful for bug reports)
- `%APPDATA%\amanece\config\config.toml` – settings (poll intervals, sound, `remote_root`, …)
- `%APPDATA%\amanece\config\known_hosts` – the server's pinned host key

## Tests

```bash
cargo test
# live checks against the real server (a few dozen read-only requests):
ALBA_USER=… ALBA_PASS=… cargo test live_ -- --ignored --nocapture
# downloads under forced session drops (resume); ALBA_KEEP=1 keeps the files to compare
ALBA_USER=… ALBA_PASS=… ALBA_DATE=… ALBA_SAMPLE=… ALBA_FRAMES_N=9 ALBA_DOWNLOAD=1 AMANECE_REKEY_MB=16 \
  cargo test --release live_app_flow -- --ignored --nocapture
```
