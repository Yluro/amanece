# Amanece

Terminal UI for the ALBA BL13-XALOC/BL6-XAIRA autoprocessing results: browse your beamtimes over SFTP,
compare the statistics of every solve (xia2chem, ednaproc, autoPROC, xia2), download the HKL
files you pick into the Local panel's folder (it starts where you launched it), and run any commands
(SHELXT, XPREP,...) on them.

Read-only, gentle and safe by design: one SFTP connection, rate-limited requests, and nothing about
your login is stored unless you tick a box: "Remember username" saves the username, "Remember
password" puts the password in the system's credential store (never in a file). 
See [DESIGN.md](DESIGN.md).

## Installation

### Prebuilt binaries

Every tagged version is published under [Releases](https://github.com/Yluro/amanece/releases)
with one archive per platform:

| Platform | Archive |
|---|---|
| Windows 10 / 11 (x64) | `amanece-<version>-x86_64-pc-windows-msvc.zip` |
| macOS, Intel | `amanece-<version>-x86_64-apple-darwin.tar.gz` |
| macOS, Apple silicon | `amanece-<version>-aarch64-apple-darwin.tar.gz` |
| Linux (x64, glibc) | `amanece-<version>-x86_64-unknown-linux-gnu.tar.gz` |

Unpack it and put `amanece` (`amanece.exe`) somewhere on your `PATH`. Each archive has a
`.sha256` file next to it.

### Updating

The version is shown at the right end of the status bar. At startup and once a day amanece asks GitHub
whether there is a newer release (one small request; nothing is downloaded). When there is, the version
turns into a green `↑ v0.4.0`: press `U` (or click it) and say `y`, and amanece downloads this platform's
archive, checks it against its `.sha256`, makes sure the new binary starts, and replaces its own file with
it. Quit and start amanece again to use the new version. `o` in that dialog opens the release notes.
`check_updates = false` in `config.toml` stops the automatic check; `U` still checks when you ask.

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
  "Remember password" needs a Secret Service keyring (GNOME Keyring or KWallet, normal on a desktop);
  without one the password just isn't saved.

### Build from source

You need [Rust](https://rustup.rs) (stable, 1.88 or newer) and a C/C++
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

The launch folder is only where the Local panel starts; amanece writes none of its own files there.
Tick **Remember username** in the login dialog (`Tab` to it, `Space`, or click) to have the username
prefilled next time. Up to 10 usernames are remembered: the last one used is prefilled and `←`/`→` in
the username field picks another. Only usernames are saved, in `login.toml`. To forget one, pick it
and press `Del` (or log in with the box unticked).

**Remember password** (off by default) also keeps that account's password, in Windows Credential
Manager, the macOS Keychain or the Linux Secret Service, under the name `amanece`; next time it is
filled in and `Enter` connects. It is saved only after the server accepted it. If the server ever
rejects a saved password (someone changed it), amanece deletes it and asks you to type the new one,
so it never retries a wrong password on its own. Untick the box and log in, or press `Del` on the
name, to remove it. Anyone using your unlocked computer account can log in with it, so think twice
on a shared machine.

Each panel shows its keys on its bottom border; `?` lists them all. In short:

- **Local** (left): browse folders, `N` makes a new folder, `Supr` moves the selected file or folder
  to the Recycle Bin (asks first). Downloads land in `<sample>/<program>/` inside the folder open
  here, like the old notebook: `JSG285b/xia2chem_r3/shelxt.hkl`, `JSG285b/autoproc_noanom_r3/…`,
  `JSG285b/frames_r3/…`.
- **ALBA**: beamtimes like folders, from XALOC (BL13) and XAIRA (BL06) together, newest first;
  `Enter` opens one (the others are hidden), `Backspace` goes back.
  A green ◆ next to a sample: it's already downloaded (from this beamtime) into the Local folder;
  yellow ◆: only a local folder with the same name exists.
- **Stats**: every solve with its statistics, ★ on the best one. The details under the table show
  how the ★ was chosen: each solve's points for completeness (30), CC½ (30), inner-shell R (20) and
  d_min (20), its place in the ranking, and for the others how far behind ★ they are and which
  criterion cost them most (or why a solve isn't ranked at all: failed, running, no HKL file).
  Values that would draw a checkCIF alert are coloured, yellow for C, orange for B and red for A:
  d_min (THETM01, sin θ/λ < 0.590 / 0.575 / 0.550), completeness (PLAT029, < 99 / 97 / 95 %) and
  Rmerge as a stand-in for Rint (RINTA01, > 10 / 15 / 20 %). The details spell each alert out.
  These come from the pipeline's overall shell, so they are a warning at the beamline, not checkCIF
  itself: the completeness runs to the pipeline's d_min (checkCIF tests θ_full, usually higher), and
  checkCIF doesn't publish the PLAT029 limits (these are read off real reports). Change the limits
  in the `[checkcif]` section of `config.toml`. `Space` marks, `d` downloads,
  `F` downloads raw frames (asks first). Frames download a few files at a time (`parallel_downloads`
  in the config, default 3) with a progress bar above the status bar; more frame datasets wait their
  turn. HKL/log downloads (`d`) jump the queue and start right away, even while frames are
  downloading. If the connection drops, it reconnects and continues where each file stopped.
  Every frame is checked against the MD5 checksum stored in its own CBF header, both after it
  arrives and when one already in the folder is skipped; a frame that fails is downloaded again.
  Under the cell, **≈ same cell** lists the samples (code and beamtime date, with their space group)
  whose cell matches, closest first, from the unit-cell database (see below).
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
sound, `E` opens the Local folder in the file explorer, `O` opens amanece's own folder (config,
cache, log), `U` updates amanece (see [Updating](#updating)), `q` quits (asks first).

## Files it writes locally

- `.alba_source.json` in each download folder – which server file each download came from

Everything else goes in one folder, `%APPDATA%\amanece\config\` on Windows (`~/.config/amanece/`
on Linux, `~/Library/Application Support/amanece/` on macOS). Press `O` in the app to open it.

- `config.toml` – settings (poll intervals, sound, `remote_root`, checkCIF alert limits, …). It is
  written once, on first run; a `config.toml` from an older version has no `[checkcif]` section and
  uses the defaults. To change them, add:

  ```toml
  [checkcif]                                 # limits for alert levels C, B, A
  sin_theta_lambda = [0.590, 0.575, 0.550]   # 1/Å, alert below (d_min 0.85 / 0.87 / 0.91 Å)
  completeness = [99.0, 97.0, 95.0]          # %, alert below
  rint = [10.0, 15.0, 20.0]                  # %, alert above (compared with Rmerge)
  ```
- `known_hosts` – the server's pinned host key
- `cache-<id>.json` – listings and stats already fetched, one file per account (`<id>` is a hash,
  not the username)
- `amanece.log` – console log (useful for bug reports)
- `login.toml` – the usernames that have "Remember username" ticked
- `cells.toml` – the unit-cell database: one entry per scanned sample (code, beamtime date, space
  group and cell of its ★ solve), from every account's cache. Add your own cells (starting materials,
  ligands, salts) as `[[cell]]` entries with a `code`, an `sg` and a `cell = [a, b, c, α, β, γ]`;
  amanece only updates the entries that have a `from` line. Cells are compared as Niggli-reduced
  primitive lattices, so other settings (P2₁/c vs P2₁/n, C2/c vs P-1, swapped axes) still match.
  The tolerances are `cell_match_pct` (edges, default 1.5 %) and `cell_match_deg` (angles, 2°) in
  `config.toml`.

Saved passwords are not in this folder: they are in the system's credential store (see above).

Older versions kept the cache and log in `.amanece/` inside the launch folder. amanece still reads
an old cache from there (so nothing is fetched again) but never writes there. Once you have logged
in with this version, you can delete those folders.

## Tests

```bash
cargo test
# live checks against the real server (a few dozen read-only requests):
ALBA_USER=… ALBA_PASS=… cargo test live_ -- --ignored --nocapture
# downloads under forced session drops (resume); ALBA_KEEP=1 keeps the files to compare
ALBA_USER=… ALBA_PASS=… ALBA_DATE=… ALBA_SAMPLE=… ALBA_FRAMES_N=9 ALBA_DOWNLOAD=1 AMANECE_REKEY_MB=16 \
  cargo test --release live_app_flow -- --ignored --nocapture
```
