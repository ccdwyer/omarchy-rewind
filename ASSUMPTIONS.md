# Assumptions

Conservative choices where the Omarchy / Quickshell / Hyprland API was not 100% certain. Uncertainty is isolated behind `RewindAdapter.qml` or a Rust module. Where the build spec and `docs/quattro-shell-reference.md` disagree, the reference wins.

## Plugin host (reference wins)

- **Entry points are `Item`s**, not `ShellRoot`. Overlay exposes `open(payloadJson)` / `close()` / `toggle()` for `omarchy-shell shell summon|hide|toggle`.
- **Injected properties:** `omarchyPath`, `shell`, `manifest`, `pluginRegistry` (and `bar` on the widget). Lookups try `pluginRegistry.serviceFor` → `shell.serviceFor` → `shell.firstPartyServiceFor`, then `omarchy-shell shell call|summon`.
- **`keepLoaded: true`** so the overlay’s layer-shell window survives between summons (image-picker pattern). The spec’s kinds/entryPoints are otherwise unchanged.
- **Settings are inline on the `shell.json` entry.** Widget keys (`byteCapGb`, `cadenceMs`, `idlePauseSec`, `excludeApps`, `titlePausePatterns`, `armOnLogin`) live in `manifest.barWidget.defaults` + `schema`. The bar widget pushes them to the service; the service sends them to rewindd. There is no plugin-owned settings file for those keys.
- **Arm/consent persistence** is runtime state, not a widget setting. rewindd writes `~/.local/share/rewind/state.json` (0600). This is capture state, not a second settings channel.
- **IPC verbs** are `omarchy-shell shell summon|hide|toggle|call <id> ...`. An extra `IpcHandler` target of the plugin id is registered; `shell call` is the documented path.
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

- Spec wants a **persistent wlr-screencopy session** via smithay-client-toolkit. That path is compiled only with `--features wayland` (wayland-client + wayland-protocols-wlr). `build.sh` tries it, then a grim-only binary, then the POSIX script.
- The Wayland module opens a connection per grab, not a long-lived event loop across ticks. Isolated in `capture_wlr.rs`. If the protocol bind is wrong on a given compositor, grim runs. Cadence floor becomes 5 s as specified for the fallback.
- Focused output comes from `hyprctl -j monitors` (`focused: true`).
- This authoring machine is macOS: wayland crates are not default features so `cargo test` stays portable.

## Encoder

- Order: `cwebp` → PNG via the `image` crate. The spec’s “image crate WebP” step needs libwebp; it is not compiled in so the binary stays dependency-light. Encoder name is recorded on every frame and in `stats`.

## Lock / idle / portal

- Lock: `pidof hyprlock`, then `loginctl show-session self LockedHint`. Hyprland `lock`/`unlock` events are logged if they fire.
- Idle: `loginctl IdleSinceHint` (µs CLOCK_REALTIME), else `hyprctl cursorpos` unchanged.
- Portal: `pw-dump` text search for xdg-desktop-portal + screencast / Stream/Input/Video. Heuristic, labeled as such.
- Overlay-open is a `set-pause` command from QML so the helper does not have to see the layer surface.

## Search / OCR

- FTS5 over app + title + clipboard is written on every frame even when OCR text is empty. If the bundled SQLite build rejects FTS5, rewindd falls back to LIKE on a regular table.
- Tesseract is optional, `nice -n 19`, TSV parse, crops deleted after. Word boxes are stored as fractions of the stored frame.

## Reopen & arrange

- Plan is built from the stored `hyprctl clients -j` vs live clients vs `.desktop` `StartupWMClass` / filename. Execution is `hyprctl dispatch exec|movetoworkspacesilent|movewindowpixel`. The overlay always shows the plan before that runs.

## Helper fallback

- The competition brief asked for a missing-binary degrade path. `compat/rewindd.sh` speaks the same NDJSON subset, grim-only, python3 for JSON, no OCR, reopen-plan returns an honest empty plan.

## Deviations from the spec (reference or honesty)

- **`keepLoaded: true`** added so the overlay outlives a summon (reference).
- **`barWidget` metadata block** added (reference requires it when `kinds` includes `bar-widget`).
- **Widget settings in shell.json**, not a Rewind config file (reference).
- **No committed 8-hour CPU% / GB/day.** Authoring host has no Hyprland. Planning numbers are the spec’s 25–80 KB/frame; live UI uses measured bytes.
- **wlr-screencopy is feature-gated**, grim is the always-on path.
- **image-crate WebP skipped** (native libwebp). PNG is the compiled fallback after `cwebp`.
- **At-rest encryption** remains a documented roadmap item (tribunal applied-changes).
