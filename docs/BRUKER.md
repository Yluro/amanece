# Bruker diffractometer integration: plan (draft for review)

Status: **plan only, nothing implemented.** Branch `claude/bruker-integration-1d1550`.
Goal: browse the group's Bruker X-ray server like ALBA: pick a person's folder, see its
experiments as samples, get unit cells from the `.p4p` files, and download frames
separated by run / matrix. Same rules as everywhere else: read-only SFTP, few requests,
no passwords stored (DESIGN.md §3, `docs/NAS.md`).

## 1. What we know about the server (from the group, not yet verified on the server)

```
/xray/data/user/
├── <member name>/          one personal folder per group member
├── guest/                  the old shared default; holds most of the samples
└── …
    └── <experiment>/       arbitrary name, usually XXXXnameYYY
        ├── *.sfrm …        frames, split into runs (and the unit-cell "matrix" run)
        └── work/           only if the sample was characterised (cell / structure)
            └── *.p4p …     cell + orientation files (also .ins/.res/.hkl/.raw …)
```

So the mapping onto amanece's model is:

| amanece        | ALBA                  | Bruker                                   |
|----------------|-----------------------|------------------------------------------|
| beamtime       | `20251211`            | a person's folder (`guest`, `jsg`, …)    |
| sample         | folder under RESULTS  | an experiment folder                     |
| run            | `<prefix>_<run>`      | a frame run (`_01_`, `_02_` …)           |
| (no equivalent)| characterisation run  | the matrix run (cell determination): a normal run with a `matrix` flag, never hidden for being matrix-only |
| solve (cell)   | autoPROC/xia2 stats   | `work/*.p4p` → unit cell                 |
| frames         | `RAW_DATA/*.cbf`      | `*.sfrm` next to the experiment          |

## 1b. How Bruker organises the files (from Bruker's own manuals, 2026-11-19)

Sources: the APEX5 user manual (DOC-M86-EXX282 V1, 04.2024) and the SAINT+ reference manual
(V8.40F, 2024), both installed with APEX under `C:\BN`; and a real experiment folder
(`3570JSG276`).

**Experiment folder.** A sample is created in APEX's *Sample Database*; APEX creates a folder for
it (default `<drive>:\frames\guest\<sample>`). "All files (e.g., frames, *.p4p files, etc.)" go
there. Sample name and folder need not match, and several samples can point to one folder;
then they all share one `work\` ("it can be helpful to rename \work folders from previous
analysis attempts") — which explains stale files in `work/`.

**Frames** (SAINT §4.1). Strict convention `*_RR_NNNN.sfrm`: base name, 2-digit run, 4-digit
frame. A "flexible" convention also exists: the last number is the frame, the one before it the
run. Runs here are `<sample>_RR_NNNN` (scans) and `matrix_RR_NNNN` (unit-cell runs). SAINT's
helper frames go to `work/`: `*_am_RR_*` (active mask), `*_ib_RR_*` (initial background),
`bg_snap_*`, and optionally `*_le_*` (long exposure), `*_xa_*` (x-ray aperture).

**work/** is created by the Integrate Images plug-in. Per run: `<name>_RR.raw` (`.mul` for twins,
`.ram` for modulated), its log `._ls`, and `._ma` (data for the global cell refinement). After
all runs: the merged output `<name>_0m.*` — `0` = all runs, `m` = merged.

**The cell files.**
- **Determine Unit Cell** (the matrix runs): "Clicking Accept stores the selected unit cell for
  further processing" — in the **Sample Database** (PostgreSQL, `C:\BN\data-9.3`), *not* in a file.
  A `.p4p` with that cell appears only when someone exports it (*Sample | Export*: `.xml` or
  `.p4p`) or runs CELL_NOW, which works from an exported `.p4p` in `work/` (manual §13.2.1).
  Such an export has `FILEID CELL_NOW` and the cell APEX held at that moment: the matrix cell
  (`work/3570JSG276.p4p`: 13.42 24.92 30.52 91.8 96.2 138.8, `BRAVAIS Triclinic P`) or, if no
  cell was determined yet, the 10/10/10/90/90/90 placeholder (RDTtest). CELL_NOW's own results
  (`FILEID C_NOW`) are named by the user (`_a.p4p`, `_b.p4p`, `.p4p_cn` …).
- **Integration** (SAINT "Output .p4p Files and Global Cell Refinement"): the constrained global
  refinement writes `<name>_0m.p4p` ("the same base name as the merged file"), the unconstrained
  one `<name>_0u.p4p` ("ends in u instead of m"); with several twin components also `_0m_a.p4p`
  and `_0u_a.p4p`. APEX: "the `\work` directory's `*m.p4p` file (containing the final unit cell
  parameters from integration)" is what space-group determination uses, and the report takes
  "Final cell constants with ESDs (from the final `*_0m.p4p` file)".
- **XPREP / structure**: `<name>_0m.prp`, `.ins`, `.hkl`; solution and refinement `.res`, `.lst`,
  `.fcf`, `.cif` in `work/` (names chosen by the user, e.g. `_0m_t1.ins`).

**Consequences for amanece (to decide, not built):**
1. The matrix cell exists as a file only when it was exported or CELL_NOW was run. Otherwise
   it is in APEX's database on the instrument PC, which amanece does not read.
2. A `.p4p` with `FILEID CELL_NOW` is *not* always a spot list: with a real cell it is the
   exported matrix cell and should be a "matrix" row; only the placeholder cell is no cell.
3. `_0m_a.p4p` / `_0u_a.p4p` (twins) belong to the integrated stage.

## 1c. What the group's NAS really holds (read-only survey, 2026-11-19)

The NAS (`<nas-host>:<port>`, the group's shared read-only account) shows the Bruker user root
at `/data/XRay`: one folder per person (nine, `guest` among them) plus a few loose files (a 2012
PDF, `.exp` files, DLLs).

**Experiments.** `guest/` holds 997 experiment folders (`1410LBRu2B`, `1646_LP94CH`,
`1659CoPr23` …). But a person folder can *itself* be an experiment: one (`<person-a>/`) holds
only matrix frames, another (`<person-b>/`) a whole experiment (3 836 `3636_RR_NNNN.sfrm`, 36
`matrix_*`, `work/`, `nac.p4p`) *next to* five experiment subfolders. These are outliers the group will
tidy up on the NAS; amanece does not handle them (decided 2026-11-19).

**Cell files in `guest/*/work/`** (1 419 `.p4p` in 609 experiments, plus 191 `.p4p_cn` in 94):

| name | count | experiments |
|---|---|---|
| `<E>_0m.p4p` | 494 | 497 |
| `<E>_0u.p4p` | 404 | 407 |
| random six-letter names (`5f9dbq.p4p`) | 319 | |
| `nac.p4p` | 66 | |
| `<E>.p4p` | 8 | |
| `<E>_01m.p4p`, `<E>_01u.p4p` (one run integrated alone) | 11 | |
| user names (`cell.p4p`, `1.p4p`, `spacestat.p4p`), twins (`_0m_a`, `_a` … `_d`) | ~20 | |

111 experiments have `.p4p` files but no `_0m`: their cell, if any, is in the other files.

**What the files are** — the `FILEID` line of 35 of them, copied read-only:

| FILEID | names seen | cell | `BRAVAIS` |
|---|---|---|---|
| `SAINT V8.34A` | `_0m`, `_01m` | integrated cell | yes |
| `APEX2 2011.8-0` | random names, `<E>.p4p` | **the cell APEX held: the matrix cell**, often after the Bravais choice (e.g. `Monoclinic(b-unique) C`) | yes |
| `Seattle` | `nac.p4p` | a real cell (2019 files) | yes, with centring |
| `CELL_NOW` | random names, 1.5–9 KB | the 10/10/10/90/90/90 placeholder: a spot list for CELL_NOW, no cell (but a real cell when exported after the cell was found, e.g. `3570JSG276.p4p` on this PC) | no |
| `C_NOW` | `.p4p_cn`, `_a`/`_b` … | CELL_NOW's result | some |

Examples: `1872_RDC301b`: APEX2 export 18.149 22.089 23.855 82.2 85.1 89.5 against SAINT `_0m`
17.994 21.914 23.662 82.0 85.4 89.6 (the matrix cell, ~0.9 % off). `1743_rdc204etoh`: APEX2
export monoclinic C 12.39 12.35 48.95 β 91.3 against `_0m` tetragonal P 8.71 8.71 46.64 (the
same lattice, another Bravais choice). APEX writes the same export several times (three
identical random files in `1111`): same-cell merging handles that.

**Consequence: classify `.p4p` by `FILEID`, not by file name** (decided and built 2026-11-19):
`SAINT` → integrated; `APEX*` (and `Seattle`, and `CELL_NOW` with a real cell) → **matrix**;
`C_NOW` → CELL_NOW; the placeholder cell → no row. This gives a matrix cell to many of the
111 unprocessed experiments, which the "only `_0m`/`_0u`" rule would have shown without a
cell. It needs one 4 KiB read per `.p4p` (capped at the 10 newest per experiment).

## 2. Facts confirmed, and what is still open

Confirmed by the group (2026-11-18):

- **Run frames:** `<name>_<run>_<frame>.sfrm` (the answer typed `.sftm`; read as a typo
  for `.sfrm`, please say if not). `<name>` may itself contain underscores
  (`3286AS264_B_01_0001.sfrm`), so the run and frame numbers are parsed **from the right**.
- **Unit-cell (matrix) runs:** `matrix_<run>_<frame>.sfrm`, i.e. the prefix is literally
  `matrix`, and there can be several matrix runs (`matrix_01_…`, `matrix_02_…`).
- **`.p4p`:** header sample in `docs/server_samples/bruker_saint.p4p` (SAINT V8.34A,
  "Integration of 3286AS264_B"). It has `CELL` + `CELLSD` and, importantly,
  `BRAVAIS Triclinic P`: the lattice system **and the centring letter**. No space group,
  exactly like SHELX `LATT` in NAS mode.
- **Access:** all group members use **one shared SFTP account**. There is no "own folder"
  to detect from the login name, so no per-user default behaviour.

Still open (blocking only where marked):

0. **Where is the cell in a matrix-only experiment?** Answered (§3.3c): *nowhere on the
   server* unless CELL_NOW or SAINT wrote a fresh file; decided: option A (§3.3c).
1. ~~Which `.p4p` to trust~~ Settled: all of them, as separate rows, no ranking (§3.3, §3.3d).
2. **Experiment name vs frame prefix.** Is the `<name>` of the run frames the experiment
   folder name? (Not blocking: the prefix is parsed per file, a folder may hold several.)
3. **Frames location.** Directly under the experiment folder (as told), never in a subfolder?
4. **Nesting.** Experiments only at `person/experiment`, or sometimes deeper? Not blocking:
   §4.2 descends lazily if needed.
5. **Connection.** Host, port, SFTP root the shared account sees (is `/xray/data/user`
   the real path or relative to a chroot?). Needed only for the live test and docs.
6. Roughly how many experiment folders `guest/` holds and how many frames a run has
   (request budget, §7).

## 3. Design

### 3.1 New server type

`ServerKind::Bruker` next to `Alba` and `Folders` (config.rs already says "a new facility
is a new variant plus its scan code"):

```toml
[[servers]]
name = "bruker"
type = "bruker"
host = "…"
port = 22
root = "/xray/data/user"     # absolute SFTP path; default /xray/data/user
```

- `Config::check_servers`: validate `root` like a folders server.
- `Config::cache_host`: `bruker:v1:<host>:<port>:<root>`; separate cache from other servers.
- Login dialog / `--type bruker` come for free from the existing server picker.
- One shared account: no login-name based behaviour. Person folders are listed
  alphabetically, `guest` like any other.

### 3.2 Remote side: one new module `remote/bruker.rs`

Same shape as `remote/nas.rs`, dispatched from `scan.rs` on `sftp.kind()`. `ReadOnlySftp`
gets a Bruker mode sharing the existing NAS path guard (`check_nas_path`: every path is
validated and its canonical form must stay under the root; symlinks may not escape).

- `list_experiments` → one listing of the root: visible, non-symlink directories become
  beamtime ids `bruker-<hash>/<person>` (same id scheme as `nas-…`).
- `list_samples(person)` → one listing; each directory is a sample **without** the trailing
  `/`. Hidden / system names (`.`, `@eaDir`, `#recycle`) skipped as in NAS mode.
- `scan_sample(person, experiment)` → see §3.3.
- `list_frames(person, experiment, runs)` → **no extra request**: the scan already
  listed the folder, so frames are listed from the scan result cached in `Run`
  (§3.4). Re-listing only when the user asks after a rescan.
- Downloads reuse `open_download` and the whole worker (resume, retries, parallelism).
  `.sfrm` has no embedded checksum, so the self-check is the size check only (the CBF
  MD5 path is keyed on `.cbf` and simply does not apply).

### 3.3 Scanning one experiment (what "look for the .p4p" means)

Requests per sample, in order, stopping early when nothing is there:

1. `list(<experiment>)`: 1 request. Gives frame names, sizes, mtimes → runs (§3.4), and
   whether `work/` exists.
2. `list(<experiment>/work)`: 1 request, only if `work/` exists. Collect every `*.p4p`,
   `*.p4p_cn`, `*.ins`, `*.res` (and note `.hkl/.raw` for phase 2); what a `.p4p` is comes from
   its `FILEID` line (§1c), not from its name.
3. `read_prefix` (4 KiB each, enough for the header up to `BRAVAIS`) of the **10 newest
   of those cell files** (§3.3b): 1–10 requests. Older ones are listed by name in a note ("22 older CELL_NOW files not read")
   and a key reads them on demand. A file whose name, size and mtime are unchanged is
   never read twice (it is in the cache).

Total 2–12 requests per sample, typically 3–5. Samples with no `work/` cost 1.

`.p4p` parser (pure, in `bruker.rs`, unit-tested on `docs/server_samples/bruker_saint.p4p`
and variants):

- `CELL a b c α β γ V` → the cell (volume ignored), validated with the same `valid_cell`
  as the NAS parser (finite, positive, angle bounds, positive volume).
- `BRAVAIS <system> <centring>` → centring letter (last token, one of `PABCIFR`). Stored as
  `space_group = "P (centring only)"`, the **same convention as NAS `LATT`**, so
  `cells::centring()` and the cell database need no change. No `BRAVAIS` line: the cell is
  kept with centring unknown (CELL_NOW files have none, §3.3c). A `BRAVAIS` line whose
  letter is no centring: no cell (an error, never a guess).
- `CELLSD` (kept for display), `SOURCE MO <λ>` (wavelength), `FILEID` (program and
  date) and `TITLE` go in the solve's note, e.g. "SAINT V8.34A 2025-07-25 · Mo".
- Malformed lines, NaN, impossible angles → no cell, a note, never a guess.

**Rows are stages, not files (decided 2026-11-19).** The Stats panel gets one row per
*stage* of the work, named like this: **matrix** (APEX's exports: `FILEID APEX2`/`APEX…`,
`Seattle`, or `CELL_NOW` with a real cell), **integrated** (SAINT's `_0m.p4p` with its `_0u.p4p`),
**final** (XPREP's `.ins` with the `.res`) and **CELL_NOW** (one row per indexing run).
Files of one stage whose cells agree (within 0.05 % on the edges and 0.05° on the angles) are
*one* row that names all its files ("same cell in …_0u.p4p"); files of one stage whose cells
differ stay separate rows. You can run CellCheckCSD or copy the cell from any row. Each row
says what it came from (files, program and version, date), so nothing is hidden.

The SAINT log (`*._ls`) is **not read** (decided 2026-11-19): it showed the matrix cell, but
as three logs of three integration attempts with the same input. The matrix stage comes from
APEX's `.p4p` exports instead (§1c).

Also left out of the runs: SAINT's own frames `*_am_*` (active mask), `*_ib_*` (initial
background), `*_le_*`, `*_xa_*` and `bg_snap_*`, which can sit next to the data. Empty `.p4p`
files (APEX's SAINT job leftovers, with `.ini` and `.slm`) give no row.

**There is no ranking, no ★ and no "best" cell** (decided 2026-11-19). Most samples have one
or two similar cells plus frames; a "test" sample is a pile of shots of many different crystals,
where "the best cell" means nothing. Rows are only ordered for display: by source, then
newest first. Nothing is chosen for you; `rank.rs` is not involved. What goes into the cell
database is described in §3.3d.

### 3.3b Cell sources by stage (from a real `work/`, 2026-11-18)

A sample's `work/` of a processed experiment (`work.zip` from the group; excerpts in
`docs/server_samples/bruker_*`) holds the cell in several files. File **stems differ**
(`3286AS264_B_0m.p4p` but `3486S264_A.ins`), so files are picked by extension, never by
matching the experiment name.

| # | file | cell | centring | when it exists |
|------|------|------|----------|----------------|
| 1 | **integrated**: `*_0m.p4p`, `*_0u.p4p` (`FILEID SAINT`) | refined on all runs together: 18.2973 18.2927 20.5019 110.018 110.074 97.740 | `BRAVAIS Triclinic P` | after SAINT |
| 2 | **final**: `*.ins` / `*.res` (SHELX; reuse the NAS parser) | final, from XPREP's chosen setting: 24.068 27.560 20.502 90 121.41 90 | `LATT 7` = C | after XPREP |
| 3 | `*.prp` (XPREP log) — **not read** | the same cell as the `.ins`/`.p4p` next to it; a row would only repeat it | – | after XPREP |
| 4 | `*.abs` (SADABS) — **not read** | one approximate-cell line, no centring: a worse copy of the `.p4p` | – | after SADABS |
| 5 | `*_0N._ls` (SAINT log) — **not read** | the starting orientation (matrix cell 18.4115 18.4506 20.7264 110.132 109.969 97.795); one log per integration attempt, so three identical rows | – | after SAINT started |
| 5b | **CELL_NOW**: `*.p4p_cn` (CELL_NOW output: `FILEID C_NOW 2008/4`, one `CELL` + `CELLSD`, no `BRAVAIS`) | cell indexed from the matrix spots; one per CELL_NOW run, random names (`0egsl6.p4p_cn`) | no | after "determine unit cell" with CELL_NOW (RDTtest has 32) |
| 6 | `.sfrm` header `CELL :` | **zero** in calibration, data and `matrix_*` frames alike (§3.3c) | – | never |

**`_0m` and `_0u`** (checked in the SAINT log of the real folder, 2026-11-19). The `0` is the run
number "all runs together" (`INTEGRATE …_0[1-4]…`; XPREP read `_0m.hkl`). The log writes
`_0u.p4p` ("New orientation is in …_0u.p4p") right after the **Unconstrained global unit cell
refinement** (crystal-system constraint 0: all six cell parameters free) and then runs the
**Constrained global unit cell refinement**, whose result is `_0m`. The two files differ in 7
of 1643 lines: the cell, its orientation matrix, `ZEROS` and one `ADCOR` line. For a triclinic
cell there is nothing to constrain, so the cells agree to the 5th digit (18.2973 / 18.2972),
and the two rows merge. For a monoclinic crystal `_0m` has α = γ = 90 exactly and `_0u` is a
few hundredths of a degree off, which may keep them as two rows. `_0m` always names the row.
(The letters `m` and `u`: `u` = unconstrained is what the log shows; what `m` stands for I have
not found written down.)

Notes:
- The cell of the final stage is the same lattice as the integrated one in another setting
  (here C2/c 24.07 27.56 20.50 β 121.4° against triclinic 18.30 18.29 20.50 110°/110°/98°);
  the rows look different but CellCheckCSD and the cell database compare reduced cells.
- Only SAINT's `_0m.p4p` and `_0u.p4p` are read among the `.p4p` files.
- The cell of CELL_NOW has **no centring**. Row 6 gives nothing.

**Can CellCheckCSD run on a matrix-only cell?** Tested locally (CellCheckCSD 1.3.0, 1.8 M
entries; 2026-11-18): the primitive cell of a C2/c entry (CSD 17227), queried with centring
`P`, finds that entry, and still does with 0.7 % edge / 0.15° angle error added to mimic a
matrix cell; the conventional cell with `C` finds it too. So a cell **without known centring
is queried as `P`** (CellCheckCSD reduces the cell itself), and the cell database /
`cells::same_lattice` already compares reduced cells. The sample from the zip itself gave no
hit with any of its cells (matrix, SAINT, XPREP): presumably a new compound.
Limit of the test: one entry. A wrong-lattice cell (e.g. a pseudo-symmetric one) may need a
second search with the XPREP/BRAVAIS centring once that file exists.

Implementation consequence (built, milestone 3): one `SolveKind` per stage (`BrukerSaintP4p`
"integrated", `BrukerShelx` "final", `BrukerCellNow`, `BrukerP4p`), parsers for `p4p`/`p4p_cn`
(CELL, BRAVAIS if present) and `ins/res` (the NAS code). The scan reads up to 10 candidate files,
merges the same-cell rows of a stage, and a rescan reads only the rows whose files changed.
Unreadable files become failed rows; CELL_NOW's placeholder cell gives no row. An unknown
centring is stored as `space_group = None` and queried as `P`.

### 3.3c Matrix-only experiments (`RDTtest`, `testJSG`; checked 2026-11-19)

Two experiments with no integration, only `matrix_01…03_0001…0012.sfrm` (3 runs × 12 frames,
about 270 KB each, ~10 MB per experiment). Findings:

- **`testJSG/work/` is empty.** Nothing but frames.
- **Frame headers carry no cell.** In all 72 frames `CELL :` and `MATRIX :` are zero
  (`docs/server_samples/bruker_matrix_sfrm_header.txt`). So §3.3b row 6 is dead. What the
  header does hold: type `Omega Scan Image`, run number, creation time, goniometer angles
  (`ANGLES` 2θ/ω/φ/χ), `START/RANGE/INCREME`, `NFRAMES`, `TARGET`, wavelengths, `DISTANC`,
  `CENTER`, 512×512 pixels, `DETTYPE`: everything an indexer needs, but not a cell.
- **`RDTtest/work/` is a trap.** It holds 90 files: for 30 random six-letter names
  (`0egsl6`…) a `.p4p`, a `.p4p_cn` and a `._cn`. They are CELL_NOW (twin-indexing) files:
  - the `.p4p` is CELL_NOW's *input* (`FILEID CELL_NOW`, 80 spots, **placeholder cell
    10 10 10 90 90 90, V = 1000**), `bruker_cellnow_input.p4p`;
  - the `.p4p_cn` is its *output* with a real indexed cell (`bruker_cellnow_output.p4p_cn`);
  - **all are dated March–October 2024**, the frames December 2025, so they are leftovers
    of other crystals, not this experiment's. A naive "newest `.p4p`" or "any `.p4p_cn`"
    would report a stranger's cell.
- The experiment's own matrix result is therefore **not on the server**. APEX keeps it in its
  project database on the instrument PC.

Rules this adds to the scan (all free, from the listing and a 2 KiB read):

1. **CELL_NOW outputs are accepted** (decided 2026-11-19): `*.p4p_cn`, `FILEID C_NOW`, one
   `CELL` line each. They carry no `BRAVAIS`, so they are stored with
   `space_group = None` ("centring unknown") and queried as `P` (§3.3b). Parsed by the same
   `CELL` parser as the SAINT p4p.
2. **CELL_NOW inputs are not cells:** a plain `*.p4p` with `FILEID CELL_NOW` (or the
   10/10/10/90/90/90, V = 1000 placeholder) is the spot list that CELL_NOW reads: no row.
   Integration `.p4p` (`FILEID SAINT`) are accepted as in §3.3.
3. **Stale files are accepted too** (decided 2026-11-19). A file older than the experiment's
   earliest frame still gets its row, with the note "dated 2024-05-21, before the frames
   (2025-12-16)" so the age is visible; nothing else changes. All 32 `.p4p_cn` in
   `RDTtest/work/` are from 2024 and the frames from December 2025, so that experiment lists
   32 CELL_NOW rows that are probably other crystals; it is a "test" sample anyway (§3.3d).
4. **Several files = several rows**, each with its own cell and its own CellCheckCSD result,
   ordered by source, then newest first. Reads are capped (§3.3, 10 newest).
5. `._cn` (CELL_NOW's text log) is never read.
6. Matrix runs stay downloadable per run, whether or not a cell file exists.

**Decision (2026-11-19): option A.** Matrix-only samples show a cell only if a CELL_NOW
(or SAINT) file exists in `work/`; otherwise "no cell on the server" and the matrix frames can be
downloaded per run to index them in APEX. Not done now, and kept as possible later work:
B (APEX exports a `.p4p` into `work/`, which the existing rules already read) and C
(indexing the matrix frames inside amanece, 1500+ lines, own cell, separate milestone).

### 3.3d "test" samples and the cell database (decided 2026-11-19)

- **Hidden by default**, exactly as in the ALBA mode: a sample whose name contains "test"
  (`layout::is_test_name`, case-insensitive; `RDTtest` and `testJSG` both qualify) is not
  listed unless `show_test = true` (the existing setting and key). Only the name decides:
  a *matrix-only* sample without "test" in its name is **shown**, because a freshly mounted
  crystal with just its matrix run is the case this integration is for.
- **Shown "test" samples** list what is there and nothing more: every cell file as a row
  (§3.3), every run (matrix and data) with frame counts, and frames downloadable per run.
  **No ★, no ranking, no entry in the unit-cell database**, and no "matching samples"
  lookup for them: a test sample holds shots of many unrelated crystals, so its cells
  must never end up as the identity of a sample. CellCheckCSD stays available by hand on
  any row, since that is a question about one cell, not a record about the sample.
  (`cells::record` already skips test names for ALBA; the same guard must cover Bruker ids,
  i.e. the `nas-` exemption in `cells::record` and `layout::beamtime_date/label` must not be
  widened to `bruker-…` ids.)
- **Ordinary samples** are not ranked either. *(Superseded 2026-11-19 by §4.2b: a sample now
  has one cell, from its highest-authority file, and that is what the database records; the
  merge below is no longer needed.)* For the cell database, rows whose cells
  describe the same lattice (within `cell_match_pct`/`cell_match_deg`, via
  `cells::same_lattice`, i.e. the sample's "one or two similar cells") are *merged* into
  one entry, labelled with the newest of them; rows with clearly different lattices
  become separate entries (`JSG285b`, `JSG285b #2`). That is de-duplication, not a choice
  of the best: all rows stay visible in the panel. **Confirmed 2026-11-19.**

### 3.4 Model changes (all `#[serde(default)]`, so existing caches still load)

- One new `SolveKind` per stage (`BrukerSaintP4p` = integrated, `BrukerShelx` = final,
  `BrukerCellNow`, `BrukerP4p`). A row is a "solve" with `stats.cell`, `status = Done`,
  `dir` = its first file's path (so `Solve::key()` is unique), `hkl` = all the files behind
  it (downloadable like an HKL), `mtime` = the newest of them, and a `note`. They carry no
  statistics and are never ranked (`rank::best` ignores them and no ★ is shown for Bruker
  samples); CellCheckCSD works on any row. The cell database records them as in §3.3d.
- `Run` gets `frames: Option<FrameSet { first, last, count, bytes, ext }>`. Each matrix run
  (`matrix_01`, `matrix_02` …) is a normal `Run` with prefix `matrix` and a new
  `matrix: bool` (serde default false). `characterisation` stays `false`, so matrix-only
  samples are neither "test shots only" nor "unprocessed" (both of those predicates skip
  matrix runs). The cell-file solves are attached to a dedicated pseudo-run `work`
  (run 0), not to a frame run. (Alternative: a `Run.cell` field; more invasive,
  rejected for now.)
- Result: hiding is by name only (§3.3d); a sample with data runs but no `work/` reads as
  "unprocessed", as on ALBA; a matrix-only sample reads as "matrix only".

### 3.5 Frame names → runs (pure functions in `layout.rs` / `bruker.rs`)

```
<name>_<run>_<frame>.sfrm        data run, e.g. 3286AS264_B_01_0001.sfrm  → prefix 3286AS264_B, run 1
matrix_<run>_<frame>.sfrm        unit-cell run, e.g. matrix_01_0001.sfrm  → prefix matrix,      run 1
```

`frame_of(file_name) -> Option<(prefix, run, frame_no)>`: strip `.sfrm`, then take the last two
`_`-separated all-digit groups as run and frame (so a name like `3286AS264_B` or
`sample_2_a` keeps its underscores and digits). Prefix `matrix` marks a matrix run (`Run.matrix`);
any other prefix is a data run. Unmatched files (`.p4p`, `.log`, images, `.sfrm` without
the two numbers) are ignored in frame lists but count in the folder's file total.
Edge cases for tests: names ending in digits (`sample_2_01_0001`), runs ≥ 100, frame
numbers wider than 4 digits, upper-case `.SFRM`.

`frame_belongs(path, prefix, run)` becomes kind-aware (today it hard-codes `.cbf`/`.h5`
and is called from `app.rs` `confirm_frames`/`enqueue_frames`). Local folders use the
existing `download::frames_tag`: `frames_r01`, and for matrix `frames_matrix_r01`.

## 4. UI

### 4.1 Reuse

The ALBA-style screen already fits (persons ≈ beamtimes, no own-folder logic: one shared account): left list = persons (as beamtimes), then experiments
(as samples), then the run/solve rows with the existing "frames" rows ticked with Space and
downloaded with `F`. Matrix appears as its own ticked row, runs as `run 01 · 360 frames ·
1.2 GB`; the frames are grouped in blocks, §4.2. The Stats panel shows the cell rows named
integrated / final / CELL_NOW (§3.3), with "centring unknown" where a file has none.

### 4.2 Frames in blocks (requested 2026-11-19)

The frames are listed in two **blocks**, each with its runs, the frame count of every run, and a
download for one run, a block, or everything:

```
matrix                       36 frames · 9.7 MB      [ticked with Space = the whole block]
  ▸ r1                       12 frames · 3.2 MB
  ▸ r2                       12 frames · 3.2 MB
  ▸ r3                       12 frames · 3.2 MB
scans                        1 080 frames · 290 MB
  ▸ r1  3286AS264_B          360 frames · 97 MB
  ▸ r2  3286AS264_B          360 frames · 97 MB
  ▸ r3  3286AS264_B          360 frames · 97 MB
```

- **Counts and sizes** come from the experiment folder's listing (`Run.frames`), so they cost
  no extra request. A run with gaps (`FrameSet::has_gaps`: 1–360 but 352 files) shows
  "352 of 360".
- **Blocks:** `matrix` = the `matrix_*` runs (`RunKind::Matrix`), `scans` = the data runs
  (`RunKind::Data`). A block row is not a run: Space on it ticks or unticks all its runs,
  Space on a run row only that run; the block row shows ◐ when only some are ticked.
- **Download:** `F` queues every ticked run, as on ALBA. A key on the block rows ("download all
  blocks") ticks everything and asks once, with the total size, before anything is queued.
- **Where files land:** `<sample>/frames_matrix_r01/…` and `<sample>/frames_r01/…`
  (`download::frames_tag`), so the two blocks never mix. A run is one confirm line: files,
  size, target folder, as `confirm_frames` does now.
- **Built when needed:** the data is already there after milestone 3 (`Run.kind`, `Run.frames`,
  `list_frames`). The block rows and their keys are milestone 4, the frame download with the
  size-only check for `.sfrm` is milestone 5.

### 4.2b Results panel for Bruker, rebuilt (requested and built 2026-11-19)

The panel's purpose is to **download what is needed to refine and finish the CIF**. Cell files
(`.p4p`, CELL_NOW) are no longer rows; they only feed the cell shown.

**Rows = reflection files.** One row per `.hkl` in `work/`, newest first:

As built (the Stats panel is ~82 columns wide, so sizes live in the Info panel and the frame
text; `P*` = only the centring is known):

As changed later the same day: **no cell row and no cell columns** — the table shows ALBA's
quality columns (from XPREP's `.prp`, or SAINT's coverage for a SAINT row); each dataset's cell is
in its Info (Cell, Similar cells, CellCheckCSD), and the sample's cell in the frame blocks' Info.
Only a sample with no dataset (matrix runs only) keeps a `Cell (matrix) · → Info` row.

```
    Data                                    SG      d_min Rmrg% Cmpl% I/σ  Mult
[ ] 3486S264_A.hkl .ins .res XPREP          C2/c    0.93  9.0   94.9  4.9  1.8
[ ] 3286AS264_B_0m.hkl SADABS               P*      0.93  9.0   94.9  4.9  1.8
[ ] 3286AS264_B_0m.raw SAINT                P*      –     –     –     –    –
[ ] matrix · 3 runs · 36 frames · 9.7 MB
[ ]   r1 · 12 frames · 3.2 MB
…
```

The first layout, superseded:

```
    Data                               a      b      c      α      β      γ SG
    Cell (final)                  24.068 27.560 20.502  90.00 121.41  90.00 C2/c
[ ] 3486S264_A.hkl .ins .res XPREP
[ ] 3286AS264_B_0m.hkl SADABS     18.297 18.293 20.502 110.02 110.07  97.74 P*
[ ] 3286AS264_B_0m.raw SAINT      18.297 18.293 20.502 110.02 110.07  97.74 P*
[ ] matrix                    3 run(s) · 36 frames · 9.7 MB
[ ]   r1                      12 frames · 3.2 MB
…
[ ] scans                     2 run(s) · 808 frames · 218.2 MB
[ ]   r1                      448 frames · 121.0 MB
```

**The cell is shown once** (2026-11-19): the sample's cell heads the table; a dataset row shows
a cell only when its own differs from it (another setting, e.g. SAINT's primitive cell next to
XPREP's C-centred one). The Data column names every file `d` fetches for the row
(`RDC78A3_test1.hkl .ins .res`; `S_01.raw +2` when the names differ). In Similar cells, NAS and
Bruker entries show their server in the date column and their folder at the end of the line.

**Every downloadable data file is a row** (2026-11-19): besides each `.hkl` (XPREP, SADABS), SAINT's
integrated reflections are listed as a **SAINT** row — the merged `*m.raw` (or `.mul` for twins), or
every run's `.raw` when there is no merged file — so SADABS/TWINABS can be run locally. Its note
says whether it was already scaled into a `.hkl`. (APEX5 manual: integration writes `.raw`/`.mul`
and `_0m.p4p`; the `.hkl` is written by the Scale step, SADABS/TWINABS. A sample with an integrated
cell and no `.hkl` was integrated but never scaled, e.g. `guest/1651Eu2D`.)

**Auxiliary files come with `L`**: `d` always fetches a row's data files (`.hkl` with its `.ins`/`.res`,
or the `.raw`/`.mul`); the auxiliary ones (`.pcf`, `.abs`, `_0m.p4p`, `._ls`, `.prp`, `.ini`) come
along only while `L` is on. The Info panel lists them as `files:` and `aux:`.

### 4.2c Data quality from the listings (requested and built 2026-11-19)

The overall figures fill the results table's d_min, Rmrg%, Cmpl%, I/σ and Mult columns, as for
ALBA's pipelines. The Info panel has a **Shells** subtab (every shell; folded at first, as for
ALBA) and a **Scaling** subtab (**Integration** on a SAINT row). Nothing is ranked or scored.

| Row | File | Shells | Scaling / Integration |
|---|---|---|---|
| XPREP, SADABS | the dataset's `.prp`, else `_0m.prp` (XPREP names it after the `.hkl` it read) | XPREP's `INTENSITY STATISTICS` table, in the space group chosen: per shell d, Cmpl%, Mult, I/σ, Rmerge%, Rsigma%; its overall and highest-resolution lines | — |
| XPREP, SADABS | the newest `.abs` | — | SADABS/TWINABS: error-model g, wR2(int) before → after refinement, data/parameter ratio, outliers rejected, Tmin/Tmax, and per run 2θ, R(int), K, reflections, I>2σ |
| SAINT | the merged `_0m._ls` (first 160 KB) | the coverage table of component 1 (cumulative Cmpl%, Redund, Rsym%; per-shell Rshell%, I/σ, %<2σ), before scaling, in SAINT's point group | spots, ⟨I/σ⟩, % below 2σ, RMS positional errors |

A per-run `._ls` is not read: its tables sit at the end, up to 1 MB in. A listing that can't be
read or parsed is left out silently. Seen on SADABS-2008/1, SAINT V8.34A and XPREP 2014/2;
newer versions on the NAS may print differently. Reads: two small files per processed sample
(cached by name within a scan), plus the start of `_0m._ls` for SAINT rows; none when `work/`
is unchanged (the stamp carries a version, now `q2`, so the first scan after this change reads them).

Keys: Space marks a dataset, ticks a frame run, or (on `matrix` / `scans`) the whole block;
`a` marks every dataset; `d` downloads the marked (or highlighted) datasets; `F` the ticked
frame runs (after a confirmation with sizes); `A` ticks every run of every block and does `F`.
Nothing is ticked to begin with (Bruker frame sets are large).

- **What an hkl is, from its name and neighbours** (real folders, §1c):
  `<stem>.hkl` with a `<stem>.ins` is XPREP's output for structure solution (`3486S264_A`,
  `3570JSG276_0m_t1`); `<E>_0m.hkl` is SADABS's corrected data (`_0m` = all runs merged).
  Twin data `*_4.hkl` / `.mul` and `_0m_uf` are listed the same way when present.
- **Cell of a row = the highest-authority file of that hkl's stem**: `.res` (SHELXL) >
  `.ins` (XPREP) > `.pcf` (XPREP) > `<stem>.p4p` (`_0m.p4p` for SADABS data). Lower files are
  ignored for that row.
- **Space group**: from the `.pcf` (`_symmetry_space_group_name_H-M 'C2/c'`) or the `.ins`/`.res`
  title (`TITL 3486S264_A in C2/c`, XPREP's convention); otherwise only the centring (`P`) from
  `LATT`/`BRAVAIS`, labelled as such.
- **Cell of the sample** (sample list, Info, cell database, CellCheckCSD): the single
  highest-authority cell in the experiment, trickling down: final (`.res` > `.ins`, with SG) >
  integrated (`_0m.p4p`, then `_0u.p4p`) > matrix (APEX export) > CELL_NOW. Everything below the
  winner is discarded, not listed. Its source is named on the "Cell" line.
- **No hkl** (matrix-only or not yet integrated): no dataset rows; the Cell line shows the matrix
  or CELL_NOW cell if there is one, else "no cell on the server"; frames as usual.

**What a dataset download brings** (into `<sample>/work/` locally, one folder per sample as on
the server, so Olex2 finds everything next to the `.ins`; a file two datasets share is fetched
once): Olex2's CIF completion (`CifInfo.py`,
`sort_out_path`) looks in the structure's folder, then its parent, for the **newest** `*.p4p`,
`*._ls`, `*.abs`, `*.pcf`, `saint.ini` and `smart.ini` and reads crystal size/colour, SAINT,
SADABS and XPREP details from them. So, per dataset row:

| file | why | which one |
|---|---|---|
| the `.hkl` | the data | the row |
| `.ins`, `.res` of the same stem | to refine | both if present |
| `.pcf` of the same stem | XPREP's CIF part: SG, cell setting, crystal | same stem, else newest |
| `.abs` | SADABS/TWINABS details for the CIF | newest in `work/` |
| `_0m.p4p` | crystal description, cell measurement (Olex2 reads the newest `.p4p`, so **only this one** is copied, never the matrix exports) | the integrated one |
| `_0m._ls` | SAINT integration details | the merged listing; if missing, the newest `_NN._ls` |
| `saint.ini`, `smart.ini` | program versions | if present in `work/` or the experiment folder |
| `.prp` (XPREP log) | for the record | same stem as the `_0m` |

`include_logs = false` keeps only `.hkl` + `.ins`/`.res` (the ALBA setting reused). Files are
small (the `.hkl` is the only one above a few hundred KB). Frames stay a separate download (§4.2).

**Scan cost**: unchanged listings; reads only the header of the files that give the cell
(`.res`/`.ins` 4 KiB, `.pcf` 4 KiB, `_0m.p4p` 4 KiB) — at most one read per dataset plus one for the
sample cell. The matrix exports and CELL_NOW files are read only when nothing higher exists.

### 4.3 Touch points (done in milestone 1: `is_folders()` is now `source_ids`, `browses_folders`, `hides_test_names`)

`is_folders()` currently conflates "trailing-slash folders" with "not ALBA". Replace it by
explicit capabilities on `ServerKind` so Bruker picks the right behaviour per feature:

| capability                     | Alba | Folders | Bruker |
|--------------------------------|:----:|:-------:|:------:|
| `samples_are_dirs_with_data`   | yes  | no      | yes    |
| `has_frames`                   | yes  | no      | yes    |
| `test_name_filter`             | yes  | no      | yes    |
| `beamtime_label_is_path`       | no   | yes     | yes (person) |
| `check_updates_ok`             | yes  | no      | yes (decide) |

Also generalise the `nas-` id prefix checks (`layout::beamtime_date/label`,
`cells::record`) into one `layout::is_folder_id` so `bruker-…` ids are handled in one place.

Nested experiment groups (§2 open item 4): if a directory in a person's folder holds no `.sfrm` and no
`work/`, but sub-directories, it is shown with a trailing `/` and opened like a NAS folder.
Decided lazily on scan (needs the one listing), not while listing the person folder.

### 4.4 Text

Header/status lines that say "beamtime/date" show "folder" for Bruker; the console prints
`↓ frames <experiment> run 01: …` exactly like ALBA.

## 5. Decisions

Settled (2026-11-18 / 19):

1. **p4p solve in a `work` pseudo-run** (not a new `Run.cell` field). §3.4.
2. **Cells without centring:** not an issue, the `.p4p` has `BRAVAIS`; stored as
   "<letter> (centring only)" like NAS. A `.p4p` lacking it gives no cell. §3.3.
3. **Own-folder default:** none; shared account.
4. **Matrix-only samples (2026-11-19):** option A of §3.3c; CELL_NOW outputs (`.p4p_cn`) are accepted as cell sources, stale ones included.
5. **Several cell files per experiment (2026-11-19):** each is its own processing row, like the ALBA pipelines, with no ranking and no ★ (§3.3). "test" samples are hidden by default and never recorded in the cell database (§3.3d).
5b. **Stages (2026-11-19):** rows are matrix, integrated, final and CELL_NOW, with same-cell
   files of a stage merged into one row (§3.3). Every `.p4p` is read and sorted by its `FILEID`
   (§1c); the SAINT log is not read. Person folders that are themselves experiments are
   outliers the group will fix; not handled.
5c. **Frames in blocks (2026-11-19):** `matrix` (r1, r2, …) and `scans` (r1, r2, …), counts per
   run, tick a run or a whole block, download all blocks (§4.2).
6. **Cell database for ordinary samples (2026-11-19):** rows with the same lattice are merged into one entry, clearly different lattices become separate entries; test samples are never recorded (§3.3d).

Open, need your call:


7. **Phase 2 files:** also offer `work/*.ins/.res/.hkl/.raw` as downloadable files (reusing
   the HKL download path and `include_logs`)? Default: not in this iteration.
8. **Update check:** NAS mode disables it (a local build would be replaced by an upstream
   binary without the feature). Same for the Bruker branch until merged.

## 6. Milestones (each ends with `cargo test`, nothing live)

| # | Step | Output |
|---|------|--------|
| 0 | Remaining facts (§2 open items 1, 3); `bruker_saint.p4p` is already in `docs/server_samples/`, a `work/` and an experiment listing are still wanted | fixtures |
| 1 | **Done (2026-11-19).** `ServerKind::Bruker` plumbing: config, `check_servers`, cache key, `bruker-<hash>` ids, capability methods (`source_ids`, `browses_folders`, `hides_test_names`) replacing `is_folders()`; ALBA and NAS tests pass untouched; login works, listing says "not implemented yet" | commit `bruker: ServerKind::Bruker…` |
| 2 | **Done (2026-11-19).** `remote/bruker.rs`, pure and tested on the real files in `docs/server_samples/`: frame names → runs and matrix runs (`frame_of`, `group_frames`), which `work/` files to read (`file_kind`, `select_candidates`, cap 10, newest first), cell files → rows (`parse_p4p` SAINT/CELL_NOW/other, `parse_shelx`, `parse_saint_log`). Also an opt-in `bruker_work_dir` test that runs them over a copied `work/` folder: on the two real folders it finds 7 cells in one and 32 CELL_NOW cells in the other, and rejects all 32 CELL_NOW inputs. `.prp` and `.abs` are not read (rows that repeat the `.ins`/`.p4p` cell); easy to add if you want them | `remote/bruker.rs` |
| 3 | **Done (2026-11-19).** `list_experiments` (1 request: person folders as `bruker-<hash>/<person>`), `list_samples` (1: every experiment folder is a sample), `scan_sample` (1 listing of the experiment, 1 of `work/`, ≤ 10 reads; unchanged files are not read again), `list_frames`; all through a small `Source` trait, so the scan is tested against an in-memory server (processed, matrix-only, empty, cap, reuse, failed rows). Model: `Run.kind` (`Data`/`Matrix`/`Work`), `Run.frames`, `Run.note`, `Solve.mtime`, five `SolveKind::Bruker*`; old caches load (tested). Matrix-only samples are neither "test shots only" nor "unprocessed". `cells::record` skips Bruker ids until milestone 4 merges rows by lattice | `remote/bruker.rs`, `model.rs` |
| 4 | **Done (2026-11-19).** Bruker results panel rebuilt (§4.2b): dataset rows (one per `.hkl`, cell + SG of its own files), the sample cell from the highest-authority file, the frame blocks (§4.2) with run/block/all ticking, dataset downloads with the Olex2 companion files into `<sample>/work/`, the cell database entry from the sample cell. Frame downloads match `.sfrm` names per run (matrix runs into `frames_matrix_rNN/`), so most of milestone 5 came along | usable in the TUI (tested with rendered screens; not yet against the live NAS) |
| 5 | Frame download path: `frame_belongs` per kind, size-only verification for `.sfrm`, "download all blocks" with one size confirmation, local folders `frames_matrix_rNN` / `frames_rNN`, resume test with the existing `.part` logic | `F` works |
| 6 | Docs (`docs/BRUKER.md` becomes the how-to, DESIGN.md §2.20), `#[ignore]` live test like `live_nas_readonly` (`BRUKER_HOST/PORT/USER/PASS/ROOT/SAMPLE`), manual run by you | review |

Milestones 1–2 are independent of server access and can start as soon as the plan is approved; 3–6 want step 0's listings.

## 7. Request budget (gentle-scraping check)

- Open person folder: 1 listing. Open experiment: 2–12 requests, usually 3–5 (§3.3), spaced by the
  existing limiter (3/s, burst 6) and background scans at one sample per 3 s.
- "Fetch all stats" on `guest/` with N experiments ≈ 3·N requests ≈ N·3 s; for N = 300 about
  15 min in the background, interruptible, cached afterwards (finished scans are reused
  until the folder's mtime changes, as for ALBA).
- Listing a folder with many frames is one SFTP readdir (several packets, one limiter
  token). Frames are never listed twice for one scan.
- No writes, no recursive walks, nothing under `work/` other than the one listing and ≤ 10
  bounded 2 KiB reads (RDTtest: 32 files → 10 reads ≈ 3 s at 3 requests/s, once; unchanged
  files are never re-read). The "read the rest" key is the only way to exceed the cap.

## 8. Risks

- Naming assumptions (§2) are unverified; mitigated by keeping them in two regexes and a
  fixture-driven test.
- `is_folders()` refactor touches many UI lines: done as its own commit with no behaviour
  change, so a regression is easy to bisect.
- The Bruker host may sit behind the university network/VPN and use another SSH setup
  (keyboard-interactive, old algorithms). The existing connect code covers password and
  keyboard-interactive; legacy key-exchange algorithms would be a separate fix.
- Frame sets are large and `.sfrm` is uncompressed; the existing download settings
  (`parallel_downloads`) apply, and the confirm dialog shows total size before anything
  is fetched.
