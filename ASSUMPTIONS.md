# Assumptions

Conservative choices where the Omarchy / Quickshell / Hyprland API was not 100% certain. Uncertainty is isolated behind `RewindAdapter.qml` or a Rust module. Where the build spec and `docs/quattro-shell-reference.md` disagree, the reference wins.

## Plugin host (reference wins)

- **Entry points are `Item`s**, not `ShellRoot`. Overlay exposes `open(payloadJson)` / `close()` / `toggle()` for `omarchy-shell shell summon|hide|toggle`.
- **Injected properties used:** `omarchyPath`, `shell`, `manifest`, `pluginRegistry`, `bar` (host load). **Not used:** `serviceFor` / `firstPartyServiceFor`. Overlay and bar widget talk to the service only through documented `omarchy-shell shell summon|hide|toggle|call <id> …`. FileView snapshots under `$XDG_DATA_HOME/rewind/` are written only while `armed && !paused`. While disarmed or paused, the bar polls `call <id> status '{}'` for live armed/paused state so it never depends on a stale `ui.json`.
- **`keepLoaded: true`** so the overlay’s layer-shell window survives between summons (image-picker pattern). The spec’s kinds/entryPoints are otherwise unchanged.
- **Settings are inline on the `shell.json` entry.** Widget keys live in `manifest.barWidget.defaults` + `schema`. The bar widget pushes them with `shell call … configure '<json>'`.
- **Arm/consent persistence** is runtime state in `~/.local/share/rewind/state.json` (0600). Steady-state disarmed operation writes nothing. The one exception is the explicit consent-transition write (`persist_consent`) when the user accepts the consent screen, including “Keep disarmed.” Capture/OCR/settings/snapshots still require armed. Startup recording is `consentAt > 0 && armOnLogin` only; a bare persisted `armed` bit does not resume capture. `arm` is rejected until consent is recorded.
- **IPC replies:** `IpcHandler` methods return a string (consumed via Process `StdioCollector waitForEnd`). Query/timeline/moment/plan payloads are also published to the snapshot files so the overlay never depends on discarded `execDetached` stdout.
- **Third-party id** is `io.github.chris.rewind`. Never `omarchy.*`.

## Quickshell

- **`Process { stdinEnabled: true; write(line) }`** is how NDJSON commands reach rewindd. Isolated in `RewindAdapter.writeLine`. A failed write is logged as `stdin-failed`; there is no second command file.
- **Overlay IPC** is a FIFO queue (`ipcQueue` / `kickIpc`) over one `Process`. `open()` issues a single `refresh` call so timeline/clips/overlay-open cannot clobber each other.
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

- Order: `cwebp` → image-crate WebP (`image` crate `webp` feature) → reduced-scale PNG (long edge 720, smaller than the 720p WebP path). Encoder name is recorded on every frame and in `stats`.

## Lock / idle / portal

- Lock: `pidof hyprlock`, then `loginctl show-session self LockedHint`. Hyprland `lock`/`unlock` events are logged if they fire.
- Idle: `loginctl IdleSinceHint` (µs CLOCK_REALTIME), else `hyprctl cursorpos` unchanged.
- Portal: `pw-dump` text search for xdg-desktop-portal + screencast / Stream/Input/Video. Heuristic, labeled as such.
- Overlay-open is a `set-pause` command from QML so the helper does not have to see the layer surface.

## Search / OCR

- FTS5 over app + title + clipboard is written on every frame (including the current clipboard text). Independent clipboard events (their own timestamps) are attached to the **nearest frame** via `record_clip_search` and a LEFT JOIN / clip-table merge so OCR-free clipboard search hits a screenshot, not an orphan ts. LIKE fallback uses the same nearest-clip-before-frame rule.
- Clipboard ingest is gated by the **full pause matrix** (not merely `armed`). While paused the in-memory clip cache is cleared so a later frame cannot attach a secret copied in KeePass etc.
- Tesseract is optional, `nice -n 19`, TSV parse, crops deleted after. Tesseract boxes are ROI-local; they are mapped through `crop_x/crop_y/crop_w/crop_h/out_w/out_h` into stored-frame 0..1 fractions. QML maps those through `Query.fittedRect` onto the PreserveAspectFit painted area.
- `wl-paste -w` payloads are NUL-delimited so multiline clipboard text is one event (64 KB cap).

## Reopen & arrange

- Plan is built from the stored `hyprctl clients -j` vs live clients vs `.desktop` `StartupWMClass` / filename. Execution is `hyprctl dispatch exec|movetoworkspacesilent|movewindowpixel`. The overlay always shows the plan before that runs.

## Helper fallback

- The competition brief asked for a missing-binary degrade path. `compat/rewindd.sh` speaks the same NDJSON protocol for query/wipe/stats/clips/timeline/moment/consent/configure, but **does not record**. A shell grim loop cannot honor the pause matrix or a persistent screencopy session; an unsafe partial recorder was rejected in review. Arm replies with `compat-norecord` and an error telling the user to run `build.sh`. Wipe rewrites `index.jsonl` / `clips.jsonl` atomically and drops missing files.

## Deviations from the spec (reference or honesty)

- **`keepLoaded: true`** added so the overlay outlives a summon (reference).
- **`barWidget` metadata block** added (reference requires it when `kinds` includes `bar-widget`).
- **Widget settings in shell.json**, not a Rewind config file (reference).
- **No committed 8-hour CPU% / GB/day.** Authoring host has no Hyprland. Frame size is a **planning estimate** of ~25–80 KB/frame (spec band), not a measurement from this machine. Live UI uses measured bytes once frames exist.
- **wlr-screencopy is feature-gated** (macOS cannot compile wayland). On Linux with the feature, the session is persistent; grim is fallback only.
- **POSIX fallback does not record** (privacy). Query/wipe of existing data still work.
- **image-crate WebP is compiled in** (`webp` feature). PNG is only the last fallback, at a smaller scale than the 720p WebP path.
- **At-rest encryption** remains a documented roadmap item (tribunal applied-changes).

## Round 9 (fable) fixes

- **Per-window Reopen** is built entirely in the helper (`plan::build_one`,
  reached via the `reopen-window` IPC command) from the desktop-file map plus
  live `hyprctl clients`. Stored window rows carry no `exec`/`cmd`, so the
  overlay never fabricates a launch command; it requests the plan async and
  shows it for confirmation before `reopen-exec`. `Plan.oneWindowPlan` was
  removed from `js/Plan.js`.
- **Arm generation guard.** `ArmState` now carries a monotonic `gen` bumped
  under the exclusive gate lock on every arm/disarm. A capture/OCR job takes a
  ticket (the generation) at start; `write_allowed`/`still_armed` require the
  ticket to still match, so a job begun before a disarm cannot commit after a
  rapid disarm→re-arm. Regression: `stale_ticket_after_disarm_rearm_cannot_commit`.
- **Privacy epoch (all pauses, not just disarm).** The arm generation only moves
  on arm/disarm, but capture must also be dropped when a NON-disarm privacy pause
  (lock, idle, overlay, portal, per-app exclusion, private-browsing, title-pattern)
  begins *during* the blocking grab/read/tesseract. A monotonic `privacy_epoch`
  is bumped on every pause-state transition (via `note_pause_change` and on
  disarm). Each capture/clip/OCR job samples both the arm ticket AND the epoch at
  entry, before blocking work; `still_recording` = ticket-match AND epoch-unchanged,
  and every commit additionally re-observes the live pause (`refresh_pause`) and
  rejects if paused now. `capture_once` re-checks immediately after the grab.
  Regressions: `capture_privacy_pause_during_grab_does_not_commit`,
  `armed_unpaused_capture_writes_a_frame` (guards against over-rejection).
- **Clip cache is generation+epoch tagged, publish-after-commit.** The last-clip
  cache holds `{text, gen, epoch}` and is populated ONLY after a clip actually
  commits; a rejected/paused clip is never cached. `latest_cached(gen, epoch)`
  returns a clip only for a frame carrying the exact same tag, and the cache is
  cleared on disarm and on every pause transition — so a clip copied in one
  recording window can never attach to a frame from another (no search-data
  leak). Regression: `clip_cache_cleared_and_not_attached_across_privacy_pause`.
- **Focused-output capture never substitutes.** When a focused output name is
  requested but has no exact wlr registry match, `pick_output` returns `None`
  (the wlr grab errors) and capture falls back to `grim -o <focused-output>` —
  it never silently records a different (possibly sensitive) display. If the
  focused monitor cannot be resolved at all (empty name), `capture_once` fails
  closed and captures nothing that tick — grim is never invoked without `-o`
  (which would grab every display) and wlr never picks the first output.
- **Idle is a capture-cadence pause, not a privacy pause.** Every HARD privacy
  pause — lock, overlay, portal/screencast, per-app exclusion, private-browsing,
  title-pattern, disarm — blocks OCR entirely: no database commit, no crop
  deletion (`ocr_may_write` rejects them, re-checked live inside the OCR
  transaction; a locked screen is treated as idle for *running* tesseract
  cheaply but never for *writing*). Idle itself is permitted for OCR by design:
  it annotates frames that were **already captured and committed while
  armed-and-unpaused** — OCR never captures new content and only ever touches
  already-authorized frames. `pending_crops()` sources timestamps exclusively
  from committed `frames` rows, and `commit_ocr_tx` additionally refuses to write
  when no committed frame row exists for that ts (also covers a frame the byte
  cap pruned between queue and commit). Regressions:
  `ocr_write_ok_blocks_locked_and_hard_pauses`,
  `ocr_only_annotates_an_existing_committed_frame`.
- **Pause/disarm perform zero mutation of *observation data*; own-orphan cleanup
  is immediate.** The zero-write contract governs OBSERVATION DATA — frames, OCR,
  clips, events, settings, and the SQLite/WAL database: none of it is written or
  mutated while paused or disarmed. Persisting new observation content IS deferred:
  pause/resume gap metadata buffers in memory (`pending_gaps`); the WAL checkpoint
  the disarm path would perform is deferred (`pending_checkpoint`, and it only ever
  merges already-committed authorized data); and a settings save that comes due
  while paused is deferred (`pending_settings_save`). These are applied only by
  `flush_deferred`, called from the capture loop **while recording** (armed and
  unpaused, re-verified via `refresh_pause`), so nothing deferred is a side effect
  of pausing/disarming.
  **Deleting a rejected capture's OWN uncommitted, unreferenced temp/staging file
  is NOT deferred — it is unlinked IMMEDIATELY, even while paused/disarmed.** That
  orphan holds screen content from the moment a pause was engaging (that is *why*
  the capture was rejected); leaving it on disk during the pause is the exact leak
  the product prevents, so cleaning up our own orphan is required privacy cleanup,
  not an observation-data write. Committed-frame file deletions (byte-cap pruning,
  wipe) run inline inside their own authorized store transaction, never via the
  pause path. Regressions: `disarm_defers_checkpoint_and_flush_applies_it`,
  `rejected_capture_temp_file_is_unlinked_immediately_even_while_paused`,
  `disarm_during_capture_discards_files_and_rows` (own temp files removed
  immediately), `settings_save_deferred_while_paused`,
  `pause_transition_does_not_write_to_store`.
- **Snapshot files are recording-only.** The QML service writes its
  `ui/timeline/clips/moment/hits/plan.json` snapshots only while `armed && !paused`
  (`persistUi`); a pause freezes them and `onPersistUiChanged` flushes the latest
  in-memory state once recording resumes. The overlay does not depend on them for
  live data (it refreshes over its own `omarchy-shell shell call` channel on
  open), so freezing them during the overlay-open pause is safe.
- **Bar reflects recording truthfully.** `toggleArm` runs through a process whose
  reply (the helper's authoritative post-toggle status) is applied directly, so
  the dot never shows "off" while recording has already started — no racing
  status poll decides the state.
- **Screencast detection fails closed.** `parse_portal_dump` matches portal
  ScreenCast sessions and common recorders (OBS, wf-recorder, gpu-screen-recorder,
  kooha, GNOME Shell) keyed on a screencast/portal marker plus a video-stream
  node — now including `Stream/Output/Video` — while ordinary video *playback*
  (same class, no marker) does not falsely pause. If `pw-dump` runs but fails, or
  spawns with any error other than not-found, `portal_screencast_active` returns
  `true` (assume sharing, pause). Only a genuinely absent `pw-dump` (no PipeWire
  tooling) yields `false`; legacy X11-only capture remains a documented limit.
  Regressions: `portal_dump_detects_output_video_screencast`,
  `portal_dump_ignores_plain_video_playback`.
- **Fallback is never mistaken for the real helper.** `build.sh` installs the
  POSIX fallback only at `compat/rewindd.sh` (plus the `rewind` CLI); it no longer
  copies it to `bin/rewindd`. So an executable `bin/rewindd` is always the real
  Rust binary, and the probe reports `fallback` (non-recording) honestly when the
  binary is absent.
- **Atomic byte-cap.** Oldest-first pruning runs inside the capture insert
  transaction (`prune_within_tx`); file unlinks happen only after commit. A
  prune error rolls back the whole capture, so a capture never reports success
  while storage is over the cap.
- **Shell fallback consent** is now written via temp file + fsync + atomic
  rename + parent-dir fsync (mirrors the Rust `Settings::save`); a failed save
  rolls back the in-memory consent and reports an error rather than claiming a
  durable write.
- **No-network audit** runs the daemon with `REWIND_TEST_CAPTURE=1`, a
  deterministic synthetic capture backend that also treats the session as
  active. The audit now fails unless a `frame-written` event (or a positive
  frame count) is observed, so the proof cannot pass without a real capture.
  This env var is used only by tests and `scripts/network-audit.sh`.
- Rust unit tests run in Linux CI; the JS harness skips them cleanly when
  `cargo` is absent (the macOS authoring host).
