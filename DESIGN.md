# amanece — design notes

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

### 2.9 XAIRA (BL06), found 2026-10-26

- Same account, same proposal folder name on both beamlines: `/bl13/projects/cycle2026-I/<digits>-<pi>` (XALOC)
  and `/bl06/projects/cycle2026-I/<digits>-<pi>` (XAIRA). After login the app looks on every beamline in
  `beamlines` (default `bl13`, `bl06`), only in cycles from the proposal's year on. Beamtime ids are then
  paths under `/` (`bl06/projects/…/20260711`); the panel shows `XAIRA 20260711`.
- `<date>/PROCESSED_DATA/<sample>/` directly (no RESULTS); samples are named by puck position (`Sample-19-01`).
- EDNA2/ewoks folders: `<prefix>_<pipeline>_<noanom|anom>_run<N>/` with pipelines `autoproc`,
  `autoproc_staraniso` (skipped), `fastdp`, `xia2dials`; plus `ref-<prefix>_testxtal_run<N>` (characterisation),
  `…_diffraction_thumbnail_run1`, `…_dozorplot_run1` (ignored). Each holds `STATUS` (`SUCCESS` / `FAIL`),
  `main_files/` (symlinks that don't resolve over SFTP), `all_files/` (autoPROC: ~800 files, never listed),
  `icat/`, `input_datasets.txt`.
- Read per solve: `STATUS`, then `all_files/autoPROC.xml` or `all_files/fast_dp.xml` (same AutoProcContainer schema,
  **R factors as fractions**). HKL: `all_files/XDS_ASCII.HKL` for both. xia2dials results not seen yet (status only).
- Raw data: `RAW_DATA/<sample>/<prefix>/` with Eiger HDF5 (`<prefix>__master.h5` + `__data_00000N.h5`, ~2 GB);
  the frames download takes every `.h5` in that folder.
- Units: only EDNA's `ISPyBStoreAutoProcv1_4_dataInput.xml` stores percentages; `ispyb.xml` (xia2),
  `autoPROC.xml` and `fast_dp.xml` store fractions. `ispyb::parse` takes the unit from the file.

### 2.10 SSH key renegotiation (found 2026-10-27)

- A frames download died at frame ~172 of a XALOC run: right when ~1 GiB had been transferred. russh rekeys
  after 1 GiB by default, and **the session drops whenever russh rekeys mid-transfer** (reproduced against ALBA
  with a 16 MiB limit: dies on the 3rd frame, with ChaCha20 and with AES-GCM alike; `live_rekey` test).
- Fix: prefer AES-GCM (OpenSSH servers rekey AES-GCM only after 64 GB, ChaCha20 after ~1 GB) and raise the
  client's rekey limit to 32 GiB (time limit 24 h). Downloads also survive a dropped session: the download loop
  asks the command loop (`ConnectionLost`), which logs in again with the in-memory credentials, and the file is
  retried. A retry resumes from the `.part` already on disk (remote seek + local append), and only tries that
  brought no new bytes count: 3 of those in a row and the file fails (its `.part` is removed then, or on cancel).
  `AMANECE_REKEY_MB=16` forces drops to test this: AS514 (9 frames + a 10.7 MB HKL, many drops) completes
  and every file's SHA-256 matches a clean run (`ALBA_KEEP=1` keeps the live test's downloads).

### 2.11 Download speed (measured 2026-10-27)

- `live_bench` (8 AS514 frames, 50 MB, read into memory, one SSH connection), release build:
  1 file at a time 10.9 MB/s; 3 at a time 11.1 MB/s; 3 at a time with a 16 MB SSH window 11.2 MB/s.
  (Debug build: 8.5 → 11.1 MB/s, so parallelism only hid debug-mode overhead.)
- The cap is the PC's link: the Intel I219-V (gigabit) negotiated **100 Mbps** ≈ 11.5 MB/s of payload. Parallel
  downloads can't help there; a gigabit link could. Re-run `live_bench` with `ALBA_PAR=1/3` on a faster network
  to see what they give.

### 2.12 Parallel downloads and priority (2026-10-27)

- **Two queues in the worker**: *express* (HKL/log downloads from the Stats panel, `d`) and *bulk* (raw
  frames, `F`). `parallel_downloads` bulk workers (config, default 3, clamped to 1–4) take express jobs
  first and bulk jobs otherwise; one more worker only ever takes express jobs, so an HKL starts at once even
  while every bulk slot is busy with frames. Live: the autoPROC HKL of AS514 (10.7 MB) arrived 3.6 s after
  queueing while 9 frames were in flight.
- All of it runs over the one SSH connection (separate SFTP file handles). Download file opens have their own
  limiter (10/s, burst 10) and still count in the request total; browsing keeps the 3/s limiter.
- If several workers lose the session at once, the command loop reconnects once: each worker sends the
  connection generation it saw (`ConnectionLost { seen }`), and a newer generation means "already reconnected".
- **Frames UI**: each run folder queued with `F` is a dataset with a one-row progress bar above the status bar
  (files, bytes, speed, ETA, failures, other datasets queued). The console gets two lines per dataset (started,
  then done / failed / cancelled), not one per frame; HKL and log downloads still get a line per file.

### 2.13 Frame checksums (2026-11-06)

- CrysAlisPro could not index JSG331 and amanece was suspected of corrupting the frames. It wasn't: all 720
  frames passed their own checksum and matched WinSCP copies byte for byte (the cause was a 90° spindle-axis
  mismatch in the instrument model). But nothing in amanece could answer "are my frames intact?", and a
  local file of the right size was trusted as is.
- Every Pilatus miniCBF carries `Content-MD5` in the MIME header of its binary section: base64 MD5 of the
  `X-Binary-Size` bytes after the `0C 1A 04 D5` marker (padding after them is not covered). `cbf.rs` checks it.
- A downloaded `.cbf` is checked before its `.part` is renamed. A mismatch removes the `.part` and the worker
  fetches the file again from scratch (a warning line in the console); after 3 such tries the file fails with
  the mismatch as its error. A `.cbf` already here with the remote size is checked too, and fetched again if it
  fails. Other files, and CBFs without `Content-MD5`, are not checked.
- `DownloadEvent::Finished` has a `verified` flag; the dataset's closing line ends in "all MD5 verified" (or
  "N MD5 verified" when only some frames carry a checksum).
- OneDrive cloud-only placeholders (Files On-Demand) are not read for the skip-existing check: reading them
  would pull every frame back from the cloud. Their size alone is trusted, as before.

### 2.14 checkCIF alerts and the ★ breakdown (2026-11-06)

- `checkcif.rs` judges a solve's overall shell against three IUCr/PLATON tests. Limits are per level
  C, B, A, in `config.toml` `[checkcif]`:
  - THETM01 (IUCr page): sin θ/λ = 1/(2·d_min) < 0.590 / 0.575 / 0.550. (0.60 is the IUCr's *target*,
    not an alert limit.)
  - PLAT029: completeness < 99 / 97 / 95 %. PLATON documents no limits, only that 0.99 or more is
    expected; real reports put 0.91–0.93 at A, 0.951–0.954 at B and 0.973–0.977 at C. checkCIF tests the
    fraction to θ_full, while the pipeline's overall completeness runs to its d_min, so ours is the stricter.
  - RINTA01 (IUCr page): Rint > 10 / 15 / 20 %. Compared with the overall Rmerge (the same sum over
    equivalents); Rmeas stands in when a pipeline reports no Rmerge (xia2chem 2026), as in the ranking.
- The table colours the d_min, Rmrg% and Cmpl% cells (yellow C, orange `Rgb(255,140,0)` B, red bold A);
  the details add one line naming each alert, or saying there is none. Failed solves get no alerts.
- `rank::breakdown` returns each criterion's fraction; the details show points earned out of 30/30/20/20
  (missing values: "–", and the rest scaled up), the bonus, the place in `rank::ranking`, and for a
  solve that isn't ★: the gap and the criterion with the largest point deficit. Unranked solves say why
  (`rank::Unranked`). The score is shown out of 100 (it was 0–1 before).
- Known gap in the ranking: d_min is the pipeline's cutoff, not where the data stop. xia2chem integrates
  JSG285b to 0.73 Å (full 20 points) with an outer shell at I/σ 0.1 and CC½ .06. The same noise shells
  push xia2chem 2026's overall Rmeas to 30–50 % on crystals where autoPROC (which cuts resolution
  itself) reports a 13–17 % Rmerge, so its Rint alert says more about the cutoff than the crystal.
  Checked against a real cache on 2026-11-06 (256 solves): completeness A alerts are mostly single
  sweeps (kappa 0) that a second sweep would fill.

### 2.15 Unit-cell database (2026-11-06)

- Feature idea "known-cell match": know at the beamline that a crystal is something already measured (a
  starting material, a ligand, the same compound from an earlier beamtime) before spending time on it.
- `cells.toml` in the config dir, one `[[cell]]` per sample: `code` (sample name), `date` (YYYYMMDD),
  `beamline` (when the beamtime id has one), `sg`, `cell`, `from` (pipeline) and an optional `note`. The cell
  is the ★ solve's (`rank::best`), else any finished solve with a cell; `test…` samples are left out. Filled
  from what amanece already has: every `cache-*.json` at startup, then every scan (`App::store_info`). No
  server requests.
- Hand-added cells have no `from`; amanece never changes an entry without one (deleting the line takes an
  entry over). Saving re-reads the file and only writes back the entries amanece changed since the last save,
  so hand edits and a second window survive. A file that doesn't parse is never written (warning at startup).
- Matching is by lattice, not by numbers: centring from the SG symbol's first letter (or ednaproc's `#n`,
  through the list of centred groups), conventional → primitive cell, Křivý–Gruber Niggli reduction with the
  epsilon handling of Grosse-Kunstleve et al. 2004. The reduced cell is unstable near its boundaries (angles
  near 90°, equal edges), so `same_lattice` also tries every basis of the other lattice built from the 13
  short vectors (coefficients −1..1, determinant ±1, all four sign patterns) and keeps the closest. Tolerances
  `cell_match_pct` (1.5 % per edge) and `cell_match_deg` (2° per angle); 50 cells all-vs-all take ~0.1 ms.
- On the real cache (50 samples) it paired JSG331 (20261023) with XAIRA's Sample-20-02 (20260711), SGX215
  (Pca2₁) with SGX215b (P2₁2₁2, axes swapped), and the six P2₁3 a ≈ 21.8 Å samples with each other.
- Stats shows `≈ same cell: CODE DATE SG, …  +N more` under the cell line, closest first, without the sample
  itself; or a grey "no match" line with the database size.

### 2.16 Version check and self-update (2026-11-06)

- The right end of the status bar shows the running version, dim. At startup and once a day amanece asks the
  GitHub API for `releases/latest` of Yluro/amanece (one unauthenticated request, far below the 60/h limit);
  `check_updates = false` in config.toml turns that off, and `U` / a click on the version still checks on demand.
  This is the only network access besides the ALBA server, and it never goes to the ALBA server.
- A newer release turns the version into a green `↑ v…` badge. `U` or a click asks first (archive name and size,
  the exe that will be replaced); `o` in that dialog opens the release page. Nothing is downloaded before `y`.
- The update (`update.rs`, a std thread with blocking `ureq`, system certificates via rustls-platform-verifier):
  download `amanece-v…-<target>.zip|.tar.gz` and its `.sha256` from the release; the SHA-256 must match; the
  binary is taken from the archive, written next to the running exe (proves the folder is writable, avoids a
  noexec `/tmp`), started once with `--version`, which must print the new version (catches e.g. a glibc too old),
  and only then swapped in with `self-replace`. The running program stays the old one until restarted: the badge
  says `v… · restart`. Any failure before the swap leaves the old exe untouched.
- Platforms without a release archive (or a release missing this platform's archive or `.sha256`) get the
  release page in the browser instead.

## 3. Gentle-scraping rules (hard requirements)

1. **Read-only by construction.** The SFTP wrapper type exposes only `read_dir`, `stat` and `open_read`.
   No mkdir / write / remove / rename / setstat are reachable from app code.
2. **One connection, one SFTP channel** for the whole app session. Reconnect with backoff (30 s → 60 s → 120 s, cap 10 min).
3. **One request queue**, processed sequentially, rate-limited (3 req/s sustained, burst 6; config can lower it, never above 5 req/s). Scanning one sample costs ≈ 26 requests (~7 s); the status bar shows the running total.
4. **Lazy:** login lists `/` (1 request). Expanding a date lists its `RESULTS/` (1). Selecting a sample discovers its
   solves and fetches their stats XMLs (≈ 10–15 requests, ~60 KB). "Fetch all stats" walks samples at ≤ 1 sample / 3 s.
5. **Cache** in the config dir, one `cache-<hash of user@host>.json` per account *(changed 2026-11-05; was
   `<launch>/.amanece/cache.json`, still read once so an upgrade fetches nothing again)*. Finished solves never change, so their stats are never fetched again.
   Unfinished solves are re-checked only by polling (1 `stat` of the marker each).
6. **Max 3 login attempts per launch**, then exit, to stay clear of lockouts.
7. **Downloads**: at most `parallel_downloads` (default 3, max 4) frames at a time plus one HKL/log slot (§2.12),
   file opens ≤ 10/s, over the same single connection. Each goes to `*.part`, then is renamed locally; a dropped
   session resumes the `.part`. Skip if the local file exists with the same size *(2026-11-06: and, for a
   `.cbf` frame, passes its own Content-MD5, §2.13)*. Raw frames only after a confirm dialog showing the total size.
8. Never list `RAW_DATA/` unless frames are requested; never list autoPROC's `<id>/` working dir.

## 4. Decisions (from the Q&A on 2026-10-22)

| Topic | Decision |
|---|---|
| Login | Dialog asks username + password at every launch. **Nothing is stored unless a box is ticked.** *(changed 2026-11-05)* A "Remember username" box, off by default, saves only the username in `<config dir>/login.toml` after a successful login. *(2026-11-06)* Up to 10 names, most recently used first: the newest is prefilled and ←→ in the username field picks another (↑↓ stay for moving between fields); the box only takes effect when a login is sent (unticked: that name is forgotten), and Del in the username field forgets the name shown without logging in; an empty list deletes the file. *(2026-11-06)* A "Remember password" box (off by default; ticking it ticks the username box, unticking the username unticks it) keeps the password in the OS credential store (`keyring` crate: Windows Credential Manager, macOS Keychain, Secret Service; service `amanece`, account `user@host`), never in a file, and only for a remembered name, saved only after the server accepted it. It is filled in next time; editing the username drops it. A saved password the server rejects is deleted at once and never retried (it would burn the 3 attempts). Unticking it and logging in, Del, or the name leaving the list deletes it. Try to connect; on failure show the error and re-prompt (max 3). `--user` flag may prefill the username. |
| What to download | The stats panel lists **every solve** with its statistics; the user marks which to download. HKL files: xia2chem `shelxt.hkl`+`.ins`, ednaproc `ep_XDS_ASCII.HKL`, autoPROC `XDS_ASCII.HKL` (anom/noanom). Logs are an optional toggle. |
| Local layout | *(changed 2026-10-23)* `<sample>/<program>/` inside the folder open in the Local panel, like the old notebook: `JSG285b/xia2chem_r3/`, `…/ednaproc_r3/`, `…/autoproc_noanom_r3/`, `…/frames_r3/`, with the server's file names. Already inside a folder named after the sample: no second level. A program folder holding files from another beamtime gets `<folder>_<date>`; nothing is overwritten. `.alba_source.json` in each program folder records the origin. |
| Console | *(changed 2026-10-23)* A terminal: console programs run in a Windows pseudo-console (ConPTY via `portable-pty`, screen kept by `vt100`), so interactive programs such as SADABS work; while one runs, each typed line + Enter is its answer, Ctrl-C interrupts (twice: kill), and its output stays in the log as text when it exits. A bare program name gets the selected file (`xprep XDS_ASCII.HKL`, `shelxt <name>`). Windows GUI programs (XPREP) start directly in their own window. |
| Polling | *(changed 2026-10-23)* Manual, 10 min, 2 min, 30 s, 5 s (hard floor 5 s). Each poll re-lists the newest and the open beamtime (1–2 small requests); the beamtime list and the rescans of new/running samples happen at most once a minute (per sample), whatever the rate. |
| Notification | Optional sound each time a new **named** (non-test) sample appears. |
| Test samples | *(changed 2026-10-27)* Only samples **named** `test…` are hidden (`t` shows them): that is known from the listing, so rows never vanish while scanning. Samples with test shots only are shown dimmed as "test shots"; datasets with no processing at all (e.g. XAIRA Sample-21-10) show as "not processed" with a frames row built from `RAW_DATA/<sample>/<dataset>/`. The highlight follows the sample when rows change. Never trigger the sound. |
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

- **Local (left):** directory browser that starts in the launch dir (its only use) and is free to navigate; marks folders that hold
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
├── config.rs          %APPDATA%\amanece\config\config.toml (poll mode, sound, external_window…; never credentials);
│                      the same dir holds known_hosts, the caches, amanece.log and login.toml
├── cache.rs           <config dir>/cache-<account hash>.json
├── cbf.rs             miniCBF self-check: binary section vs its Content-MD5 (§2.13)
├── cells.rs           unit-cell database <config dir>/cells.toml, Niggli reduction, lattice matching (§2.15)
├── checkcif.rs        checkCIF-style alerts from the overall shell (§2.14)
├── rank.rs            ★ best-solve score, its per-criterion breakdown and the ranking (§2.7, §2.14)
├── local.rs           local directory model
├── remote/
│   ├── session.rs     russh connect + password/keyboard-interactive auth, TOFU host-key check
│   ├── sftp.rs        ReadOnlySftp newtype (read_dir / stat / open_read only)
│   ├── worker.rs      command loop (listing, reconnect/backoff) + download workers (express/bulk queues,
│   │                  resume, CBF check, progress events)
│   ├── layout.rs      path building + discovery for both layouts, regexes, test-sample detection
│   └── ispyb.rs       AutoProcContainer parser + unit normalisation
├── download.rs        job → local path mapping, .part/rename, skip-existing, provenance file
├── console.rs         command runner, external-window launcher
├── poll.rs            poll scheduler, new-sample diff, sound (MessageBeep, BEL fallback)
├── update.rs          version check against GitHub releases, verified self-update (§2.16)
└── ui/                local.rs, alba.rs, stats.rs, console.rs, dialogs.rs (login, confirm), statusbar.rs
```

UI thread never blocks: workers talk to `App` over `tokio::mpsc` channels.

Crates (pin current versions at implementation time): `ratatui` + `crossterm`, `tokio`, `russh` + `russh-sftp`
(pure Rust, works on the MSVC toolchain, supports the server's ECDSA host key), `quick-xml`, `serde`/`serde_json`/`toml`,
`regex`, `clap`, `directories`, `anyhow`, `tracing` (file log `amanece.log` in the config dir), `windows-sys` (sound).

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

1. Username: "never store it" read as *nothing* written to disk, with an optional `--user` flag. [yes; since 2026-11-05 also an opt-in "Remember username" box]
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
  colleague can just run the single `amanece.exe`.
- When handing the .exe to colleagues, tell them: Windows 10 1809+ or 11, 64-bit; unsigned, so SmartScreen asks
  once (*More info → Run anyway*); best in Windows Terminal (old console may show boxes for ★ ✓ ⏎); SHELX /
  XPREP / SADABS on PATH only for the console; their own ALBA login. Consider a zipped GitHub release.
