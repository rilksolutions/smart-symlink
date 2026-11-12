# DirLinker

Move folders to another drive and leave a link behind, so programs keep working.
Dual-pane GUI (Tauri) plus a keyboard-driven terminal version, sharing one Rust core.

## Layout

| Path | What |
|---|---|
| `crates/core` | All file logic: drives, listing, link detection, folder sizes, link-type rules, move + link |
| `src-tauri` | GUI backend (thin Tauri command layer over the core) |
| `src/` | GUI frontend (TypeScript, no framework) |
| `crates/tui` | Terminal UI (`dirlinker-tui`, ratatui) |

## Run / build

```bash
npm install
npm run app:dev                         # GUI with hot reload
npx tauri build                         # release GUI + installer (NSIS on Windows, deb/AppImage on Linux, dmg on macOS)
npx tauri build --no-bundle             # just the exe: target/release/dirlinker.exe
cargo run -p dirlinker-tui --release    # terminal version (starts in the current folder)
cargo test -p dirlinker-core            # integration tests (real junctions/links in temp folders)
```

## How "Move & link back" works

1. Same drive: rename (instant), then create the link at the original path.
2. Different drives: copy → verify file count and bytes → rename the original to `<name>.dirlinker-old`
   (fails if a program has files open: the copy is removed and nothing changes) → create the link → delete the old copy.

Every step before the final delete rolls back on failure. Deleting never follows links or junctions.

## Link types offered

| | Windows | Linux | macOS |
|---|---|---|---|
| Junction | folders, local drives, no admin | – | – |
| Symbolic link | needs Developer Mode or admin | ✓ | ✓ |
| Hard link | files, same volume | files, same filesystem | files, same volume |
| Copy-on-write clone | ReFS / Dev Drive | Btrfs, XFS, bcachefs | APFS |
| Bind mount | – | root, folders | – |
| Btrfs snapshot | – | subvolumes only | – |

Availability is decided per selection from the real volume capabilities (Windows volume flags,
`/proc/self/mounts` on Linux), and unavailable types show why.

## Terminal keys

Source: `↑↓`/`jk` move · `Enter`/`l` open · `Backspace`/`h` up · `Space` select · `a` all · `s` sort · `r` reverse · `.` hidden ·
`Enter`/`Tab` with a selection → destination · `q` quit

Destination: navigate the same way · `t` link type · `m` move vs. link-only · `Ctrl+Enter` (or `Ctrl+J` / `F2`) link · `Esc` back
