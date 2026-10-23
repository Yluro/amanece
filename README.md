# alba_watch

Terminal UI for the ALBA BL13-XALOC autoprocessing results: browse your beamtimes over SFTP,
compare the statistics of every solve (xia2chem, ednaproc, autoPROC, xia2), download the HKL
files you pick into the folder you launched from, and run SHELXT / XPREP on them.

Read-only and gentle by design: one SFTP connection, rate-limited requests, nothing is ever
written on the server, and credentials are never stored. See [DESIGN.md](DESIGN.md).

## Build and run

```bash
cargo install --path .        # puts `alba_watch` on your PATH (~/.cargo/bin)
cd <folder where the data should go>
alba_watch                    # or: alba_watch --user u2025xxxxxxx
```

Each panel shows its keys on its bottom border; `?` lists them all. In short:

- **Local** (left): browse folders, `N` makes a new folder, `Supr` moves the selected file or folder
  to the Recycle Bin (asks first). Downloads land in `<sample>/<program>/` inside the folder open
  here, like the old notebook: `JSG285b/xia2chem_r3/shelxt.hkl`, `JSG285b/autoproc_noanom_r3/…`,
  `JSG285b/frames_r3/…`.
- **ALBA**: beamtimes like folders; `Enter` opens one (the others are hidden), `Backspace` goes back.
- **Stats**: every solve with its statistics, ★ on the best one. `Space` marks, `d` downloads,
  `F` downloads raw frames (asks first).
- **Console**: a terminal (Windows ConPTY). `:` to type. Commands run in the Local folder and a
  bare program name gets the selected file: with `XDS_ASCII.HKL` selected, `xprep` runs
  `xprep XDS_ASCII.HKL`. Interactive console programs work: while SADABS (or any program) runs,
  what you type plus Enter is its answer; Ctrl-C interrupts, twice kills. XPREP and other Windows
  GUI programs open in their own window.
- **Mouse**: click a panel or row, double-click to open, click a Stats checkbox to mark, wheel to
  scroll, click the status badges (poll / sound / logs) to toggle. Shift+drag still selects text
  in Windows Terminal.

`Tab` cycles panels, `p` cycles polling, `n` toggles the new-sample sound, `q` quits (asks first).

## Files it writes locally

- `.alba_source.json` in each download folder – which server file each download came from
- `<launch folder>/.alba_watch/cache.json` – listings and stats already fetched
- `<launch folder>/.alba_watch/alba_watch.log` – console log (useful for bug reports)
- `%APPDATA%\alba_watch\config\config.toml` – settings (poll intervals, sound, `remote_root`, …)
- `%APPDATA%\alba_watch\config\known_hosts` – the server's pinned host key

## Tests

```bash
cargo test
# live checks against the real server (a few dozen read-only requests):
ALBA_USER=… ALBA_PASS=… cargo test live_ -- --ignored --nocapture
```
