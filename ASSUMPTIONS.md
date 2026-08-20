# Assumptions

Conservative choices where the Omarchy / Quickshell / Hyprland API was not 100% certain. Uncertainty is isolated behind `RewindAdapter.qml` or a Rust module. Where the build spec and `docs/quattro-shell-reference.md` disagree, the reference wins.

## Plugin host (reference wins)

- **Entry points are `Item`s**, not `ShellRoot`. Overlay exposes `open(payloadJson)` / `close()` / `toggle()` for `omarchy-shell shell summon|hide|toggle`.
- **Injected properties used:** `omarchyPath`, `shell`, `manifest`, `pluginRegistry`, `bar` (host load). **Not used:** `serviceFor` / `firstPartyServiceFor`. Overlay and bar widget talk to the service only through documented `omarchy-shell shell summon|hide|toggle|call <id> …` plus FileView snapshots the service writes under `$XDG_DATA_HOME/rewind/` (`ui.json`, `timeline.json`, `clips.json`, `moment.json`, `hits.json`, `plan.json`).
- **`keepLoaded: true`** so the overlay’s layer-shell window survives between summons (image-picker pattern). The spec’s kinds/entryPoints are otherwise unchanged.
- **Settings are inline on the `shell.json` entry.** Widget keys live in `manifest.barWidget.defaults` + `schema`. The bar widget pushes them with `shell call … configure '<json>'`.
- **Arm/consent persistence** is runtime state in `~/.local/share/rewind/state.json` (0600). Startup recording is `consentAt > 0 && armOnLogin` only; a bare persisted `armed` bit does not resume capture. `arm` is rejected until consent is recorded.
- **IPC replies:** `IpcHandler` methods return a string (consumed via Process `StdioCollector waitForEnd`). Query/timeline/moment/plan payloads are also published to the snapshot files so the overlay never depends on discarded `execDetached` stdout.
- **Third-party id** is `io.github.chris.rewind`. Never `omarchy.*`.

## Quickshell

- **`Process { stdinEnabled: true; write(line) }`** is how NDJSON commands reach rewindd. Isolated in `RewindAdapter.writeLine`. If `write` is missing, the adapter also appends to `cmd.ndjson` (daemon does not currently watch that file — documented last-ditch, not a second protocol).
- **`stdout: SplitParser { onRead }`** is the documented line reader (same family as snitch’s Socket parser).
- **`PanelWindow` + `WlrLayershell`** (`Overlay` layer, exclusive keyboard, `ExclusionMode.Ignore`) matches Desktop Undo / clipboard. Namespace `rewind`.
- **Theme tokens** `Color.menu.*`, `Color.accent`, `Style.*`, `Border.*`, `BarWidget`, `WidgetButton`, `BorderSurface` — same first-party set as Desktop Undo. Reduced motion: `Style.reduceMotion` or `OMARCHY_REDUCED_MOTION=1`.
- **`Hyprland.rawEvent`** is used only as a lock/unlock hint. Pause truth for lock/idle/exclusion/portal lives in rewindd (`pidof hyprlock`, `loginctl`, `hyprctl -j clients`, `pw-dump`).
- **`Hyprland.dispatch`** is used from the adapter for reopen-exec fallback; rewindd also runs `hyprctl dispatch` itself. If `dispatch` throws, `hyprctl` is spawned.
- **No invented Quickshell APIs.** Capture, encode, OCR, SQLite, and clipboard watching are all in rewindd.

## Capture

- Spec wants a **persistent wlr-screencopy session**. `CaptureSession` in `capture.rs` owns a long-lived `capture_wlr::Session`: Wayland `Connection`, registry, `zwlr_screencopy_manager_v1`, outputs, event queue, and a reusable wl_shm pool/buffer. Each tick only creates a new `zwlr_screencopy_frame_v1` and `copy`s into the existing buffer (recreated only if size/stride change). Compiled with `--features wayland`. `build.sh` tries that, then a grim-only binary, then the POSIX script.
- Grim remains **fallback only** (connect failure, grab failure, or no wayland feature). Grim cadence floor is 5 s.
- Unchanged dHash backs the next tick off to 10 s (`capture::next_cadence_ms`).
- Focused output comes from `hyprctl -j monitors` (`focused: true`).
- This authoring machine is macOS: wayland crates are not default features so `cargo test` stays portable. No Linux prebuilts are claimed.

## Encoder

- Order: `cwebp` → PNG via the `image` crate. The spec’s “image crate WebP” step needs libwebp; it is not compiled in so the binary stays dependency-light. Encoder name is recorded on every frame and in `stats`.

## Lock / idle / portal

- Lock: `pidof hyprlock`, then `loginctl show-session self LockedHint`. Hyprland `lock`/`unlock` events are logged if they fire.
- Idle: `loginctl IdleSinceHint` (µs CLOCK_REALTIME), else `hyprctl cursorpos` unchanged.
- Portal: `pw-dump` text search for xdg-desktop-portal + screencast / Stream/Input/Video. Heuristic, labeled as such.
- Overlay-open is a `set-pause` command from QML so the helper does not have to see the layer surface.

## Search / OCR

- FTS5 over app + title + clipboard is written on every frame (including the current clipboard text). Independent clipboard events (their own timestamps) are attached to the **nearest frame** via `record_clip_search` and a LEFT JOIN / clip-table merge so OCR-free clipboard search hits a screenshot, not an orphan ts. LIKE fallback uses the same nearest-clip-before-frame rule.
- Clipboard ingest is gated by the **full pause matrix** (not merely `armed`). While paused the in-memory clip cache is cleared so a later frame cannot attach a secret copied in KeePass etc.
- Tesseract is optional, `nice -n 19`, TSV parse, crops deleted after. Word boxes are stored as fractions of the stored frame; QML maps them through `Query.fittedRect` onto the PreserveAspectFit painted area.
- `wl-paste -w` payloads are NUL-delimited so multiline clipboard text is one event (64 KB cap).

## Reopen & arrange

- Plan is built from the stored `hyprctl clients -j` vs live clients vs `.desktop` `StartupWMClass` / filename. Execution is `hyprctl dispatch exec|movetoworkspacesilent|movewindowpixel`. The overlay always shows the plan before that runs.

## Helper fallback

- The competition brief asked for a missing-binary degrade path. `compat/rewindd.sh` speaks the same NDJSON protocol for query/wipe/stats/clips/timeline/moment/consent/configure, but **does not record**. A shell grim loop cannot honor the pause matrix or a persistent screencopy session; an unsafe partial recorder was rejected in review. Arm replies with `compat-norecord` and an error telling the user to run `build.sh`. Wipe rewrites `index.jsonl` / `clips.jsonl` atomically and drops missing files.

## Deviations from the spec (reference or honesty)

- **`keepLoaded: true`** added so the overlay outlives a summon (reference).
- **`barWidget` metadata block** added (reference requires it when `kinds` includes `bar-widget`).
- **Widget settings in shell.json**, not a Rewind config file (reference).
- **No committed 8-hour CPU% / GB/day.** Authoring host has no Hyprland. Planning numbers are the spec’s 25–80 KB/frame; live UI uses measured bytes.
- **wlr-screencopy is feature-gated** (macOS cannot compile wayland). On Linux with the feature, the session is persistent; grim is fallback only.
- **POSIX fallback does not record** (privacy). Query/wipe of existing data still work.
- **image-crate WebP skipped** (native libwebp). PNG is the compiled fallback after `cwebp`.
- **At-rest encryption** remains a documented roadmap item (tribunal applied-changes).
