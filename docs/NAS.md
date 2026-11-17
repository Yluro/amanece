# Generic NAS archive mode (local branch)

This opt-in mode reuses amanece's password login, host-key checking, read-only
SFTP connection, downloads and local unit-cell database. The default ALBA mode
is unchanged. No NAS installation, Docker service or scheduled task is needed:
each employee runs the client on their computer.

## Connect

Build with a current stable Rust toolchain:

```sh
cargo build --locked --release
./target/release/amanece --nas --host NAS_HOST --port NAS_PORT \
  --user amanece --remote-root /data/XRay
```

Use `--remote-root /Public/ALBA` for the other archive, or choose a smaller
subdirectory when browsing a large archive. These are **SFTP paths**; Synology's
filesystem paths `/volume1/...` are not the paths exposed to this account.
Only one archive root is open per process. Alternatively set `nas = true` and
`remote_root` in the existing `config.toml`. Passwords do not belong in that file
or in command-line arguments; enter the account password in the login dialog.

Employees need the NAS host, forwarded port, account name and password, archive
path, and the trusted SSH host-key fingerprint. Keep the NAS account's server-side
read-only permissions in place. Router source-IP restrictions are optional and
configured separately from the application; removing them does not remove SSH
authentication or grant write access.
Distribute a verified `known_hosts` entry before first login, using amanece's
existing format (not OpenSSH's public-key format):

```text
NAS_HOST:NAS_PORT ssh-ed25519 SHA256:VERIFIED_FINGERPRINT
```

On macOS this file is `~/Library/Application Support/amanece/known_hosts`.
Do not overwrite other entries. Upstream uses trust on first use for unknown
keys; verifying/preinstalling the key avoids trusting a new key blindly.
Automatic update checks are disabled in NAS mode to avoid replacing the local
build with an upstream binary that lacks this feature.

## Browse and download

The NAS panel shows the root's immediate subfolders as archive groups. Open a
group to list only its immediate subfolders and supported metadata files.
Enter opens a subfolder; Left/Backspace goes to its parent. No recursive scan
runs when opening a folder, so a large tree does not block navigation. Empty
folders and listing failures have explicit messages; `r` retries a failed listing.
Metadata files directly in the root appear in a separate **Archive root** group.
A sample row is one metadata file, preserving separate cells even when several
crystals share a folder. `.ins` and `.res` variants remain separate entries;
they can appear as similar-cell matches.

Select a file to read its cell; `S` fetches metadata for the group's unscanned
files in that directory (not its subfolders), `s` rescans the selected file, and `r` refreshes folder listings. Already
indexed archive files may change: use `s` to refresh their metadata. The existing
cell database and matching UI work with these records, and caches are separated
by mode, host, port and root. Archive source IDs also keep cell database entries
from different sources separate. Matching covers files already scanned locally,
not all files on the NAS automatically.

Supported metadata:

- SHELX `.ins` / `.res`: `CELL` and `LATT`. Centring is explicitly labelled as
  such; a full space group is not inferred from a title.
- `XDS_ASCII.HKL` / `ep_XDS_ASCII.HKL`: unit cell and space-group number in the
  header.

The archive result has no fabricated processing statistics or quality score.
The existing file-download action retrieves the selected metadata file and, for
SHELX, a same-stem `.hkl` companion if present. Download destinations use the
existing local naming/provenance rules; this is not a mirror of the NAS tree.
No raw-frame discovery or bulk frame download is offered in this mode.

## Limits and verification

Each metadata scan reads at most 64 KiB, using the existing request limiter.
Hidden/system directories and symlinks are skipped. Resolved paths are checked
against the selected root. Server-side permissions remain the security boundary.
Each folder opening issues just a path check and one directory listing, independent
of the size of its descendant tree. Inaccessible directories are reported as
errors rather than leaving an indefinite loading indicator. Nested folder caches
survive a refresh of the archive root. The old recursive-list cache is kept in a
separate namespace and is not reused by this version.

CIF-only datasets, zipped archives and arbitrary detector formats are not indexed
in this initial patch. Missing/invalid cells or unknown centring are reported,
not guessed. The existing local terminal/file-management features remain local;
this NAS feature exposes no remote mutation operations.

```sh
cargo test --locked
# Optional integration test using a temporary loopback SFTP server, synthetic data:
python -m pip install paramiko
python tests/nas_sftp_fixture.py
```

The loopback test verifies password authentication against a pinned key, recursive
listing, SHELX and XDS cells, local cell indexing, download reads, skipped symlinks
and the absence of write-mode file opens. Real-server verification uses the ignored
`live_nas_readonly` test. Supply `NAS_HOST`, `NAS_PORT`, `NAS_USER`, `NAS_PASS`,
`NAS_ROOT`, `NAS_GROUP`, `NAS_SAMPLE`, and `NAS_KNOWN_HOSTS` in its environment.
`NAS_GROUP` is empty for metadata directly in the root; otherwise it names one
immediate subdirectory. `NAS_EXPECT_CELL` can hold six comma-separated numbers.
Never commit credentials or laboratory data fixtures.
