# alba_watch — design notes

A ratatui TUI that replaces the `alba_download.ipynb` notebook (ALBAscrapper): browse the
ALBA BL13-XALOC autoprocessing results over SFTP, compare the statistics of every solve,
download the HKL files (and optionally logs / raw frames) you pick into the directory
you launched from, and run SHELXT / XPREP on them.

Status: milestones 1–7 implemented (2026-10-22), tested live with both account types. Everything under "Server findings" was
observed with a handful of read-only SFTP sessions on 2026-10-22 (only `ls` and a few
`get`s of files ≤ 15 KB). Sample files from that session are in `docs/server_samples/`
and are meant as parser test fixtures.

---

## 1. What the old notebook did (and why it hurt)

- Hard-coded username, password, experiment date and an absolute local base path.
- Assumed one layout: `/<date>/PROCESSED_DATA/RESULTS/<sample>/<pipeline>_<sample>_<run>_<n>/proc_<id>/…`.
  That layout changed by late 2025 (see §2.2), so the notebook no longer finds anything new.
- Downloaded every autoPROC (anom + noanom) and ednaproc `XDS_ASCII.HKL` for every sample, blind.
  No statistics, so no way to choose the good solve.
- Folder counter `i` is shared across pipelines, so `autoPROC_anom_1`, `ednaproc_3`… numbers mean nothing.
- Filtered samples only by `"test" in name`; wrote a `sample_list.txt` you edited by hand.
- Ignored xia2 / xia2chem completely, though xia2chem already produces SHELXT-ready files.

## 2. Server findings

### 2.1 Connection

| | |
|---|---|
| Host | `userdata.cells.es`, **port 443** (SFTP, not HTTPS) |
| Auth | password (OpenSSH offered `password,keyboard-interactive`; implement both) |
| Host key | `ecdsa-sha2-nistp256` |
| Account | one per proposal (`u<proposal number>`), shared by the whole group |
| Root `/` | one folder per beamtime date (`20250329` … `20251211`), plus `DATA/` (empty) and dotfiles |
| `~/.ssh` | owned by root, so users can't install their own public key |

On the beamline file system the same tree lives under
`/beamlines/bl13/projects/cycle<YYYY-I|II>/<proposal>-<pi>/<date>/…`; paths inside the ISPyB XMLs use
that prefix, so strip everything up to `/<date>/` to map them back to SFTP paths.

### 2.2 Layout (two variants)

```
/<date>/
├── RAW_DATA/<sample>/[<prefix>/]<prefix>_<run>_<frame:04>.cbf      ~6.2 MB each, e.g. 720 frames ≈ 4.5 GB
├── PROCESSED_DATA/<sample>/[<prefix>/]characterisation_…/           EDNA strategy inputs only
└── PROCESSED_DATA/RESULTS/<sample>/[<prefix>/]<solve dirs>          ← what we browse
```

- **Old (≤ 2025-07):** no `<prefix>` level; solve dirs are named after the sample:
  `RESULTS/AS268_kappa0/autoproc_AS268_kappa0_1_1/`, `…_2_1/`
- **New (2025-12):** a `<prefix>` level (the MXCuBE file prefix, here `manually-mounted`), and solve dirs are named
  after the prefix: `RESULTS/JSG285b/manually-mounted/xia2chem_manually-mounted_3_1/`
- `RESULTS/` only holds samples that got some processing; `RAW_DATA/` and `PROCESSED_DATA/` list every sample.

**Discovery rule:** list `RESULTS/<sample>`; if entries match the solve-dir regex it is the old layout, otherwise
each entry is a prefix dir: list it once.

```
solve dir:  ^(autoproc|ednaproc|xia2chem|xia2)_(?P<prefix>.+)_(?P<run>\d+)_(?P<n>\d+)$
char. dir:  ^characterisation_(?P<prefix>.+)_run(?P<run>\d+)_\d+$
```

Each solve dir contains exactly one `proc_<id>/` (one listing needed to learn the id).

### 2.3 Pipelines: where things are (relative to `proc_<id>/`)

| Solve | Stats XML (ISPyB `AutoProcContainer`) | HKL to download | Optional logs | "Finished" marker |
|---|---|---|---|---|
| autoPROC noanom | `ISPyBStoreAutoProcv1_4_noanom/ISPyBStoreAutoProcv1_4_dataInput.xml` (10 KB) | `AutoPROCv1_0_noanom/<id>/XDS_ASCII.HKL` (~20 MB) | `AutoPROCv1_0_noanom/summary.html`, `report.pdf`, `autoPROC.xml` | `ISPyBStoreAutoProcv1_4_noanom/…_dataOutput.xml` exists |
| autoPROC anom | same with `_anom` | `AutoPROCv1_0_anom/<id>/XDS_ASCII.HKL` | same with `_anom` | same with `_anom` |
| ednaproc | `ISPyBStoreAutoProcv1_4/…_dataInput.xml` (15 KB; a second `ISPyBStoreAutoProcv1_4-00000003/` also exists, use the `<anomalous>` flag) | `results/ep_XDS_ASCII.HKL` (~10 MB) | `results/ep_*_CORRECT.LP`, `ep_*_aimless_*.log` | `results/.finished` exists |
| xia2chem (small-molecule mode) | `Xia2DIALSv1_0_noanom/ispyb.xml` (5 KB) | `Xia2DIALSv1_0_noanom/DataFiles/shelxt.hkl` + `shelxt.ins` (~5.6 MB, HKLF 4 + CELL/SYMM/SFAC) | `xia2.txt`, `xia2-summary.dat`, `xia2.html` | `ispyb.xml` exists |
| xia2 (protein mode) | `Xia2DIALSv1_0_noanom/ispyb.xml` | none (only `.mtz` / `.sca`): stats only | same as xia2chem | `ispyb.xml` exists |

autoPROC also writes `_staraniso` variants (anisotropic cut-off, protein-oriented): hidden by default.

- The autoPROC `<id>` dir (e.g. `157860/`) is a huge XDS working dir (64 KB directory entry). **Never list it.**
  Get `<id>` from the parent listing (the numeric dir next to `<id>.dat`) and fetch `<id>/XDS_ASCII.HKL` directly.
- ednaproc's `results/` contains a zero-byte file whose **name** is `<SG number>_<a>_<b>_<c>_<α>_<β>_<γ>`
  (e.g. `75_28.387_28.387_14.359_90.0_90.0_90.0`). So one listing gives SG and cell for free.
- Results land ~10–15 min after the sweep (JSG285b: collected 10:55, xia2chem 11:05, autoPROC 11:08).

### 2.4 Statistics: one parser for all pipelines

All four write the ISPyB `AutoProcContainer` schema (`docs/server_samples/*_ispyb.xml`):
`spaceGroup`, `refinedCell_{a,b,c,alpha,beta,gamma}`, then three `AutoProcScalingStatistics` blocks
(`overall`, `innerShell`, `outerShell`) with `resolutionLimitLow/High`, `completeness`, `multiplicity`,
`meanIOverSigI`, `rMerge`, `rMeasAllIPlusIMinus`, `rPimAllIPlusIMinus`, `ccHalf`, `anomalousCompleteness`,
`nTotalObservations`, `nTotalUniqueObservations`; plus image range in `AutoProcIntegration`.

This XML is preferred over autoPROC's `summary.html` (132 KB, autoPROC-only, HTML scraping). `summary.html` /
`xia2.html` stay available as optional log downloads to open in a browser.

Gotchas:
- **Rmerge units differ:** EDNA and autoPROC store percent (`8.9`); xia2 stores a fraction (`0.085`). Normalise to %.
- Numbers come as `2.839000e+01` (EDNA/autoPROC) or `28.450001` (xia2).
- Tag case differs: `ntotalUniqueObservations` (EDNA) vs `nTotalUniqueObservations` (xia2). Match case-insensitively.
- EDNA and autoPROC wrap the container in `<XSDataInputStoreAutoProc>`, xia2 does not; search for
  `AutoProcContainer` at any depth.

### 2.5 Account types and the 2026 layout (found 2026-10-22, second pass)

- **Two kinds of accounts.** Older proposal accounts (`u2024…`) log straight into their proposal folder, so
  `/` holds the beamtime folders. Newer ones (`u2025…`) log into the whole beamline tree (`/bl01` … `/bl31`);
  their data is in `/bl13/projects/cycle<YYYY>-I/<account digits>-<pi>/`. After login the app resolves
  `realpath .`; if that has no `YYYYMMDD` folders but has `bl13/`, it lists `bl13/projects/`, then the
  `cycle*` folders newest first, and picks the folder starting with the account digits (usually 3 listings).
  `remote_root` in the config overrides the detection. The app maps its `/<date>/…` paths onto that root.
- **2026 beamtimes** keep the ≤ 2025 `RESULTS/<sample>/<pipeline>_<sample>_<run>_<n>/proc_<id>/` layout.
  Next to `RESULTS`, `PROCESSED_DATA/<sample>/` now holds the job folders (Slurm script, `input.yml`, an
  80-byte `*Output_*.xml`; `workarea: SCRATCH`) plus new `dozor_`, `mosflm_`, `xds_` and `<sample>_runN_1`
  folders, all ignored.
- **Beamtime without RESULTS** (20260514): samples are listed from `PROCESSED_DATA/` instead ("flat").
  A solve folder without `proc_<id>` and without result files counts as failed once its job output exists
  or nothing changed for 3 h.

### 2.6 May 2026 notes

- Same RESULTS layout as March 2026, but many 20260513 samples failed server-side: autoPROC leaves
  `ISPyBStoreAutoProcv1_4_{anom,noanom}_failed/`, xia2 leaves `xia2-error.txt`, ednaproc's `results/` has no
  `ep_XDS_ASCII.HKL`. The app shows these as failed with that reason.
- xia2chem's ISPyB XML no longer reports `rMerge`, only `rMeasAllIPlusIMinus`; the table shows Rmeas with a `*`
  and the ranking uses it.

### 2.7 UI decisions (2026-10-23)

- The ALBA panel is a folder browser: beamtimes, and inside one beamtime only its samples behind `../`.
- ★ marks the best solve of a sample: score = 0.3·completeness + 0.3·CC½ + 0.2·(1 − inner-shell R/20 %)
  + 0.2·resolution (1 at ≤ 0.80 Å, 0 at ≥ 1.20 Å), only finished solves with HKL files; see `src/rank.rs`.
- Every panel shows its useful keys on its bottom border; the console is compact and grows while focused.

### 2.8 Terminal and mouse notes

- `portable-pty` opens ConPTY with `PSEUDOCONSOLE_INHERIT_CURSOR`, so ConPTY first asks `ESC[6n` and waits for
  the cursor-position reply; the reader answers it from the vt100 screen, **in one write** (a split reply is read
  as separate keys and the program never starts).
- The pseudo-console is 200 rows tall at the panel's width; the panel shows the log followed by the rows up
  to the cursor, and resizes the width with the window.
- Mouse capture is on (crossterm); the last frame's panel/list geometry (`app::Hit`) maps clicks to rows and to
  the status-bar badges.

## 3. Gentle-scraping rules (hard requirements)

1. **Read-only by construction.** The SFTP wrapper type exposes only `read_dir`, `stat` and `open_read`.
   No mkdir / write / remove / rename / setstat are reachable from app code.
2. **One connection, one SFTP channel** for the whole app session. Reconnect with backoff (30 s → 60 s → 120 s, cap 10 min).
3. **One request queue**, processed sequentially, rate-limited (3 req/s sustained, burst 6; config can lower it, never above 5 req/s). Scanning one sample costs ≈ 26 requests (~7 s); the status bar shows the running total.
4. **Lazy:** login lists `/` (1 request). Expanding a date lists its `RESULTS/` (1). Selecting a sample discovers its
   solves and fetches their stats XMLs (≈ 10–15 requests, ~60 KB). "Fetch all stats" walks samples at ≤ 1 sample / 3 s.
5. **Cache** in `<base>/.alba_watch/cache.json`. Finished solves never change, so their stats are never fetched again.
   Unfinished solves are re-checked only by polling (1 `stat` of the marker each).
6. **Max 3 login attempts per launch**, then exit, to stay clear of lockouts.
7. **Downloads are sequential**, one file at a time, to `*.part` then renamed locally. Skip if the local file exists
   with the same size. Raw frames only after a confirm dialog showing the total size.
8. Never list `RAW_DATA/` unless frames are requested; never list autoPROC's `<id>/` working dir.

## 4. Decisions (from the Q&A on 2026-10-22)

| Topic | Decision |
|---|---|
| Login | Dialog asks username + password at every launch. **Nothing is stored.** Try to connect; on failure show the error and re-prompt (max 3). `--user` flag may prefill the username. |
| What to download | The stats panel lists **every solve** with its statistics; the user marks which to download. HKL files: xia2chem `shelxt.hkl`+`.ins`, ednaproc `ep_XDS_ASCII.HKL`, autoPROC `XDS_ASCII.HKL` (anom/noanom). Logs are an optional toggle. |
| Local layout | *(changed 2026-10-23)* `<sample>/<program>/` inside the folder open in the Local panel, like the old notebook: `JSG285b/xia2chem_r3/`, `…/ednaproc_r3/`, `…/autoproc_noanom_r3/`, `…/frames_r3/`, with the server's file names. Already inside a folder named after the sample: no second level. A program folder holding files from another beamtime gets `<folder>_<date>`; nothing is overwritten. `.alba_source.json` in each program folder records the origin. |
| Console | *(changed 2026-10-23)* A terminal: console programs run in a Windows pseudo-console (ConPTY via `portable-pty`, screen kept by `vt100`), so interactive programs such as SADABS work; while one runs, each typed line + Enter is its answer, Ctrl-C interrupts (twice: kill), and its output stays in the log as text when it exits. A bare program name gets the selected file (`xprep XDS_ASCII.HKL`, `shelxt <name>`). Windows GUI programs (XPREP) start directly in their own window. |
| Polling | *(changed 2026-10-23)* Manual, 10 min, 2 min, 30 s, 5 s (hard floor 5 s). Each poll re-lists the newest and the open beamtime (1–2 small requests); the beamtime list and the rescans of new/running samples happen at most once a minute (per sample), whatever the rate. |
| Notification | Optional sound each time a new **named** (non-test) sample appears. |
| Test samples | Names containing `test`, or samples with only characterisation runs, are hidden; one key shows them. Never trigger the sound. |
| Raw frames | Runs listed in the stats panel, full sweeps pre-selected, characterisation runs not; confirm dialog with total GB. |

## 5. UI

```
┌ Local  C:\…\beamtime ───────┐┌ ALBA  20251211 ─────────┐┌ JSG285b · run 3 · img 1–720 ──────────────────────┐
│ ../                          ││ ▾ 20251211      45       ││     Solve            SG  d_min Rmrg% CC½  Cmpl I/σ  │
│ JSG285b/                     ││   ● JSG285b      ✓✓✓✓    ││ [x] xia2chem         P4  0.73   8.5 .998 99.9  9.1  │
│   xia2chem_r3/               ││   ● JSG285-2     ✓✓…✓    ││ [ ] ednaproc         P4  0.94   7.2 .997 100  …     │
│ AS268_kappa0/                ││   ● JSG287       ✓✓✗✓    ││ [ ] autoPROC noanom  P4  0.86   8.9 .998 100  10.4  │
│                              ││ ▸ 20251106               ││ [ ] autoPROC anom    P4  …                          │
│                              ││ ▸ 20250704               ││     xia2             P4  …            (stats only)  │
│                              ││                          ││ cell 28.45 28.45 14.39 90 90 90                    │
│                              ││                          ││ frames  [x] run 3  720 × 6.2 MB   [ ] run 2 (char) │
└──────────────────────────────┘└──────────────────────────┘└────────────────────────────────────────────────────┘
┌ Console  cwd .\JSG285b\xia2chem_r3 ─────────────────────────────────────────────────────────────────────────────┐
│ > shelxt shelxt                                                                                                  │
│ > _                                                                                                              │
└──────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
 poll: slow 10m (next 07:42) · ♪ on · queue 2 · ↓ 12.3 MB/s · ? help
```

- **Local (left):** directory browser rooted at the launch dir but free to navigate; marks folders that hold
  downloaded solves. Its current dir is the console's working dir.
- **ALBA (middle):** dates → samples only. Per-sample glyphs: one per solve (✓ done, … running, ✗ failed).
- **Stats (right):** solves of the selected sample (all runs), Enter on a row expands overall/inner/outer shells.
  Below 160 columns it moves under the ALBA panel.
- **Console (bottom):** input line + scrollback.

Keys (draft): `Tab`/`Shift-Tab` cycle panels · `↑↓`/`jk` move · `Enter` open/expand · `Backspace` up (local) ·
`Space` mark solve/run · `d` download marked HKLs · `L` toggle "include logs" · `F` download marked frames ·
`s` fetch stats for the selected sample · `S` fetch all (throttled) · `r` refresh · `t` show/hide test samples ·
`p` cycle poll mode · `n` sound on/off · `:` focus console · `Ctrl-C` kill running command · `?` help · `q` quit.

## 6. Console runner

- Line is run in the local panel's dir (`cmd /C <line>` on Windows) with stdout/stderr piped into the scrollback.
  `Ctrl-C` kills it; `↑↓` history; builtins `cd`, `clear`.
- Programs in `external_window` (default `["xprep"]`) are spawned with `CREATE_NEW_CONSOLE` so they get their own
  interactive window; the console just logs "started (pid …)" and the exit code.
- SHELX is on PATH here (`C:\Program Files\ShelX`: shelxt, shelxl, xprep, …).

## 7. Architecture

```
src/
├── main.rs            CLI (clap), terminal setup/teardown, event loop
├── app.rs             App state, focus, key dispatch, messages from workers
├── config.rs          %APPDATA%\alba_watch\config.toml (poll mode, sound, external_window…; never credentials)
├── cache.rs           <base>/.alba_watch/cache.json
├── local.rs           local directory model
├── remote/
│   ├── session.rs     russh connect + password/keyboard-interactive auth, TOFU host-key check
│   ├── sftp.rs        ReadOnlySftp newtype (read_dir / stat / open_read only)
│   ├── worker.rs      tokio task: job queue, rate limiter, reconnect/backoff, progress events
│   ├── layout.rs      path building + discovery for both layouts, regexes, test-sample detection
│   └── ispyb.rs       AutoProcContainer parser + unit normalisation
├── download.rs        job → local path mapping, .part/rename, skip-existing, provenance file
├── console.rs         command runner, external-window launcher
├── poll.rs            poll scheduler, new-sample diff, sound (MessageBeep, BEL fallback)
└── ui/                local.rs, alba.rs, stats.rs, console.rs, dialogs.rs (login, confirm), statusbar.rs
```

UI thread never blocks: workers talk to `App` over `tokio::mpsc` channels.

Crates (pin current versions at implementation time): `ratatui` + `crossterm`, `tokio`, `russh` + `russh-sftp`
(pure Rust, works on the MSVC toolchain, supports the server's ECDSA host key), `quick-xml`, `serde`/`serde_json`/`toml`,
`regex`, `clap`, `directories`, `anyhow`, `tracing` (file log in `.alba_watch/`), `windows-sys` (sound).

Data model sketch:

```rust
struct Experiment { date: String, samples: Option<Vec<Sample>> }
struct Sample     { name: String, is_test: bool, runs: Vec<Run>, scanned: Option<SystemTime> }
struct Run        { prefix: Option<String>, run: u32, solves: Vec<Solve>, frames: Option<FrameInfo> }
struct Solve      { kind: SolveKind, remote_dir: String, proc_id: u64, status: Status, stats: Option<Stats> }
enum   SolveKind  { AutoProc { anom: bool }, EdnaProc, Xia2, Xia2Chem }
struct Stats      { space_group: String, cell: [f64; 6], overall: Shell, inner: Shell, outer: Shell, images: (u32, u32) }
struct Shell      { d_low: f64, d_high: f64, completeness: f64, multiplicity: f64, i_sig: f64,
                    rmerge_pct: f64, rmeas_pct: Option<f64>, rpim_pct: Option<f64>, cc_half: f64 }
```

## 8. Milestones

1. Skeleton: panels, focus, local browser, config, help overlay.
2. Login dialog + SFTP worker + read-only wrapper; list dates and samples; cache.
3. Solve discovery (both layouts) + ISPyB parser (tests on `docs/server_samples/`) + stats panel.
4. HKL / optional log downloads with progress; local layout; skip-existing.
5. Console runner + XPREP in a new window.
6. Poll modes + new-sample sound.
7. Raw-frame download with run picker and size confirmation.

## 9. Still open (defaults in brackets)

1. Username: "never store it" read as *nothing* written to disk, with an optional `--user` flag. [yes]
2. Sound only on a new named sample, or also when its first solve finishes (~10–15 min later)? [new sample only]
3. xia2chem files: keep `shelxt.hkl/.ins` or rename to `<sample>.hkl/.ins`? [keep original names]
4. Same sample name in two beamtimes (e.g. `MR8`): suffix the solve folder with `_<date>` on collision? [yes]
5. Other programs to open in their own window besides XPREP (olex2, shelxle, platon)? [xprep only]
6. Windows only? [yes; sound and new-window are behind `cfg(windows)`]

Not yet verified on the server: RAW_DATA layout for old (≤ 2025-07) beamtimes; what a *failed* pipeline leaves
behind; the meaning of ednaproc's second `ISPyBStoreAutoProcv1_4-00000003` dir; which of password /
keyboard-interactive the server actually accepted.

## 10. TODO

- **Portable .exe (static C runtime).** The release build imports `VCRUNTIME140.dll` (Visual C++
  Redistributable), so it won't start on a PC without it ("VCRUNTIME140.dll was not found"). Build with
  the CRT linked in: `.cargo/config.toml` with
  `[target.x86_64-pc-windows-msvc] rustflags = ["-C", "target-feature=+crt-static"]`, then check that the
  only imports left are Windows' own DLLs (`api-ms-win-crt-*` / UCRT ship with Windows 10+). After that a
  colleague can just run the single `alba_watch.exe`.
- When handing the .exe to colleagues, tell them: Windows 10 1809+ or 11, 64-bit; unsigned, so SmartScreen asks
  once (*More info → Run anyway*); best in Windows Terminal (old console may show boxes for ★ ✓ ⏎); SHELX /
  XPREP / SADABS on PATH only for the console; their own ALBA login. Consider a zipped GitHub release.
