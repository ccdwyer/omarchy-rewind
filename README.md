# Rewind

A local time machine for your desktop. Arm it, then scrub, search, and recover any recorded moment. Everything stays under `~/.local/share/rewind/`. Nothing leaves the machine.

This is an Omarchy shell plugin (service + overlay + bar-widget). It runs inside the long-lived `omarchy-shell` process. It does not start a second Quickshell instance.

Recording starts **disarmed**. The bar chip is the truth: if it says disarmed, rewindd is not writing frames.

## Install

```sh
omarchy plugin add <git-url> --enable
```

That is all for a normal Omarchy (Arch) machine: **on first run the service builds the recorder itself.** If `bin/rewindd` is absent and `cargo` is present (Arch ships Rust), the service runs `build.sh` in the background — the bar shows *"building recorder…"* — and starts the compiled `rewindd` when it finishes. So `omarchy plugin add … --enable`, then arm from the consent screen, and recording works.

You can also build it ahead of time:

```sh
~/.config/omarchy/plugins/io.github.chris.rewind/build.sh
```

`build.sh` compiles `rewindd` (Rust). Missing Wayland headers usually still produce a **grim-only Rust helper**. The POSIX `compat/rewindd.sh` fallback is used only when `cargo` is unavailable or the build fails. That fallback **does not record** (it cannot honor the pause matrix), but it does answer query/wipe/stats **against the real SQLite store** (`rewind.db`) — so a wipe there actually deletes your data, it never reports a false success. Recording itself requires the Rust binary.

Linux CI (`.github/workflows/linux-helper.yml`) builds the Wayland helper, runs tests, `ldd`, and `strace -e trace=network`, and uploads `rewindd-linux`. This macOS authoring host cannot produce that ELF; `scripts/network-audit.sh` exits 1 when `bin/rewindd` is missing rather than pretending the audit passed.

`omarchy plugin enable io.github.chris.rewind` places the chip in `barWidget.defaultSection` (`right`). To move it later:

```sh
omarchy bar move io.github.chris.rewind right
# or: omarchy bar set …
```

Reload plugins if the shell was already running:

```sh
omarchy-shell shell rescanPlugins
```

Optional garnish, never required:

```sh
# better stills than PNG
# cwebp
# optional OCR (idle, nice 19, crops only)
# tesseract
```

## Usage

| Action | How |
|---|---|
| Arm / disarm | Click the bar chip |
| Open the overlay | Right-click the chip, or the bind below |
| Clipboard history | **Long-press** the chip, then Enter to re-copy |
| Wipe | Overlay “wipe today”, or `./scripts/rewind wipe today\|all\|range` |

The plugin does **not** write Hyprland binds. Add them yourself:

```
bind = SUPER, R, exec, omarchy-shell shell summon io.github.chris.rewind '{}'
bind = SUPER SHIFT, R, exec, omarchy-shell shell call io.github.chris.rewind toggleArm '{}'
```

In the overlay:

| Key | Action |
|---|---|
| `/` | Search (titles, clipboard, OCR if present) |
| `←` / `→` | Step one frame |
| `Shift+←` / `Shift+→` | Step about a minute |
| `Home` / `End` | First / last frame |
| `y` | Re-copy that moment’s clipboard |
| `r` | Reopen & arrange — review the plan, then confirm |
| `Esc` | Close |

First launch opens a consent screen. Nothing is pre-checked that records. “Keep disarmed” is the default button. “Arm on login” is off until you check it.

## What it records (only while armed)

- Focused output, about every 3 s (5 s on the grim fallback). Unchanged frames (perceptual hash) drop to 10 s and are not written again.
- 720p-ish still: `cwebp` → image-crate WebP → reduced-scale PNG if both WebP paths fail. Full-res focused-window crop kept for idle OCR, then deleted.
- Window class, title, workspace, and a `hyprctl clients -j` layout snapshot.
- Clipboard text only, 64 KB cap, via one persistent `wl-paste -w` child.

**0 fps** when: disarmed, session locked (`hyprlock` / loginctl), idle > 2 min, this overlay is open, an excluded app is **visible on any output**, a screencast portal session looks active, or a top-3-browser private-window title matches (heuristic, labeled in the UI).

Default exclusions: KeePassXC, 1Password, Bitwarden, Seahorse, polkit agents.

## Disk math (honest)

Planning estimate (not measured on this host): a 720p still of text-heavy UI is **~25–80 KB/frame**, not single-digit KB. Retention is a **byte cap** (default 2 GB) covering **all** retained observation data — frames, crops, clipboard, layouts, and the OCR/search index — not just frame bytes. When the cap is exceeded, the **oldest window across every table** (frames, clips, layouts, events, OCR) is pruned inside the same transaction, and its files are removed via durable deletion tombstones so an interrupted unlink can't leave an untracked screenshot behind.

The bar and consent screen report **≈N days at your usage** once rewindd has measured this machine. Until then they show the planning band for 25–80 KB at a 10 s write average (dHash skip), not a promised calendar window.

This plugin was authored on macOS without Hyprland or Quickshell. Hyprland/Quickshell runtime, a fresh-VM install, soak, and QML validation are **runtime-unverified on this host**. After you arm it, `rewindd stats` prints frames, bytes, and the encoder actually used. `scripts/network-audit.sh` prints `ldd` and, on Linux, runs `strace -e trace=network` through a capture cycle.

## 60-second demo (self-contained)

1. Install, `build.sh`, arm from the consent screen or the chip.
2. Run `scripts/seed-demo.sh` (opens two windows, types `zebra-token-rewind-demo`, copies a git hash). Wait ~20 s.
3. Summon the overlay. Scrub. Search the phrase. Long-press the chip, Enter — paste the hash. Hit `r` on an earlier frame, read the plan, confirm.
4. Click the chip. Disarmed. Capture stops.

## Settings

Inline on the `shell.json` bar entry (no plugin config file):

| Key | Default | Meaning |
|---|---|---|
| `byteCapGb` | `2` | Retention cap |
| `cadenceMs` | `3000` | Capture interval (grim floor is 5 s) |
| `idlePauseSec` | `120` | Idle pause |
| `excludeApps` | password managers + polkit | Class substrings, visible-anywhere |
| `titlePausePatterns` | `""` | Extra title/class pause needles |
| `armOnLogin` | `false` | Only honored after consent |

## CLI

User-facing launcher (plugin-relative; `build.sh` also installs `bin/rewind`):

```
./scripts/rewind wipe today
./scripts/rewind wipe all
./scripts/rewind wipe range --from MS --to MS
./scripts/rewind query TEXT
./scripts/rewind stats
```

The service starts `bin/rewindd` as the NDJSON daemon. Do not invoke a bare `rewindd` unless that helper is on your `PATH`.

Data lives at `$XDG_DATA_HOME/rewind` (default `~/.local/share/rewind`), created with umask `0077`.

## Honest limitations

- **Armed-on-demand, not ambient.** Disarmed means zero *observation-data* writes — even if consent and history already exist. No new frames, OCR, clips, pause events, or settings reach disk until you arm; persistent storage is opened only after consent/arm. (Two deliberate exceptions that are *not* observation: cleaning up a rejected capture's own orphan temp file happens immediately — leaving screen content on disk during a pause would be worse — and the overlay's read-response channel lives in the ephemeral tmpfs runtime dir, not persistent storage, so browsing your history while paused serves already-recorded data without capturing anything new.) “Arm on login” is opt-in after consent.
- **Focused output only.** Other monitors are not captured.
- **Reopen & arrange is a reviewable plan**, not session restore. Missing apps launch by `.desktop` mapping; browser tabs, documents, and unsaved state are listed as unrecoverable.
- **Clipboard is text only.** Images and passwords in password-manager windows should never be captured because those apps pause recording while visible — still, do not arm Rewind over a password field in a terminal.
- **OCR is optional.** Without tesseract, search is titles + clipboard + app names. That path is the one designed to demo well.
- **Screencast pause** reads the live PipeWire graph (`pw-dump`): it detects portal ScreenCast sessions and the common recorders (OBS, wf-recorder, gpu-screen-recorder, kooha, GNOME Shell) across `Stream/Input/Video` and `Stream/Output/Video` nodes, and **fails closed** — if `pw-dump` errors, capture pauses. The only gap is legacy X11-only screen capture (no PipeWire node); for that, pause by opening the overlay or disarming.
- **Private-window pause is heuristic** on documented Firefox / Chrome / Brave title markers.
- **Lock/idle** prefer the compositor + `pidof hyprlock` + `loginctl`. The grim fallback cannot open a wlr-screencopy session.
- **No at-rest encryption.** Roadmap, not v1. Wipe anytime.
- **No second Quickshell process.** Helper is a rust binary talking NDJSON on stdio. The POSIX `compat/rewindd.sh` is a non-recording protocol stub, not a second capture path.
- **Keybinds are yours.**
- **CPU% / GB/day** are not fictionalized. Measure on device; the daemon records per-frame bytes.

## Privacy proofs

- `scripts/network-audit.sh` — `ldd` of `rewindd` plus `strace -e trace=network` on a capture cycle (Linux).
- Perms: umask `0077`, directories `0700`, files `0600`. Unit-tested.
- Disarmed ⇒ capture loop does not encode or insert.

## License

MIT. See [LICENSE](LICENSE).
