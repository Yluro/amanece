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

Press `?` inside the app for all keys. In short: `Tab` cycles panels, `Space` marks a solve,
`d` downloads, `F` downloads raw frames (asks first), `:` types in the console, `p` cycles
polling, `q` quits.

Downloads land in `<sample>/<pipeline>_r<run>/` (e.g. `JSG285b/xia2chem_r3/shelxt.hkl`).

## Files it writes locally

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
