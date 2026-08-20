# Assumptions

Conservative choices where the Omarchy / Quickshell / Hyprland API was not 100% certain. Uncertainty is isolated behind `RewindAdapter.qml` or a Rust module. Where the build spec and `docs/quattro-shell-reference.md` disagree, the reference wins.

## Plugin host (reference wins)

- **Entry points are `Item`s**, not `ShellRoot`. Overlay exposes `open(payloadJson)` / `close()` / `toggle()` for `omarchy-shell shell summon|hide|toggle`.
- **Injected properties used:** `omarchyPath`, `shell`, `manifest`, `pluginRegistry`, `bar` (host load). **Not used:** `serviceFor` / `firstPartyServiceFor`. Overlay and bar widget talk to the service only through documented `omarchy-shell shell summon|hide|toggle|call <id> …`. The read-response snapshot channel (`ui/timeline/clips/moment/hits/plan.json`) lives in the **ephemeral tmpfs runtime dir `$XDG_RUNTIME_DIR/rewind`** (per-user 0700), never persistent storage; if `XDG_RUNTIME_DIR` is unset the channel fails closed (empty path) rather than fall back to an insecure `/tmp` path. These are transient views of already-authorized data, so they publish whenever `consent && (armed || overlayOpen)` and the overlay reads them even while its own privacy pause is active. New observation capture remains gated on `armed && !paused`.
- **`keepLoaded: true`** so the overlay’s layer-shell window survives between summons (image-picker pattern). The spec’s kinds/entryPoints are otherwise unchanged.
- **Settings are inline on the `shell.json` entry.** Widget keys live in `manifest.barWidget.defaults` + `schema`. The bar widget pushes them with `shell call … configure '<json>'`.
- **Arm/consent persistence** is runtime state in `~/.local/share/rewind/state.json` (0600). Steady-state disarmed operation writes nothing. The one exception is the explicit consent-transition write (`persist_consent`) when the user accepts the consent screen, including “Keep disarmed.” Capture/OCR/settings/snapshots still require armed. `armOnLogin` is not persisted, so the daemon boots disarmed; startup auto-arm is deferred until the shell pushes `armOnLogin` (with `consentAt > 0`) via `configure`. A bare persisted `armed` bit does not resume capture. `arm` is rejected until consent is recorded.
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

- The competition brief asked for a missing-binary degrade path. `compat/rewindd.sh` speaks the same NDJSON protocol for query/wipe/stats/clips/timeline/moment/consent/configure, but **does not record**. A shell grim loop cannot honor the pause matrix or a persistent screencopy session; an unsafe partial recorder was rejected in review. Arm replies with `compat-norecord` and an error telling the user to run `build.sh`. Query/timeline/moment/clips/stats and wipe operate on the REAL `rewind.db` SQLite store (via `python3`'s bundled `sqlite3`), returning absolute frame paths and deleting the actual frame/ocr/clip/layout/event rows plus their files — never a legacy JSONL index. A wipe never reports success while data survives.

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
- **Snapshot files are an ephemeral read channel, not observation storage.** The
  service publishes `ui/timeline/clips/moment/hits/plan.json` into the tmpfs
  runtime dir (`$XDG_RUNTIME_DIR/rewind`, 0700; fail-closed if unset) whenever
  `consent && (armed || overlayOpen)`. The overlay and bar read from there. Because
  these are transient renderings of already-committed authorized data — never new
  captured content, and never persistent storage — serving them while the overlay's
  own privacy pause is active is not an observation write. New capture stays gated
  on `armed && !paused`.
- **Bar reflects recording truthfully (fire-and-forget).** `toggleArm` is sent to
  the daemon fire-and-forget; the chip renders **only** from the daemon-authoritative
  `ui.json` snapshot plus the `status` poll. It never applies an optimistic or
  pre-toggle reply, so the dot can't show "off" while recording is on or flip before
  the daemon acknowledges.
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

## Round 15 review

- **Overlay read-response channel (snapshots) is separate from capture
  gating.** Opening the overlay pauses *capture* (we must not record the overlay
  itself), but the overlay still needs to show your history. The six snapshot
  files (timeline/clips/moment/hits/plan/stats) are published whenever there is a
  consented consumer — while recording **or** while the overlay is open — and
  they live in the ephemeral tmpfs runtime dir (`$XDG_RUNTIME_DIR/rewind`, 0700,
  cleared on logout), not the persistent data dir. They carry only
  already-authorized data served back to the UI, never new observation, so this
  does not weaken "zero observation-data writes while paused." A fresh,
  never-consented install publishes nothing.
- **Bar truth on disarm.** `Service.disarm()` no longer optimistically flips
  `armed`/`paused`; the chip changes only when the helper's authoritative `state`
  event arrives. So the dot never shows "disarmed" while recording is still on,
  and never after a failed disarm write (`send` failure leaves armed=true).
- **Fresh-install recorder (3 tiers).** On first run the service provisions the
  recorder without a required toolchain: (1) an existing executable `bin/rewindd`
  is used as-is; (2) else if `cargo` is present it builds via `build.sh` (bar
  shows "building recorder…"); (3) else if `curl`/`wget` + a sha256 tool are
  present, `scripts/fetch-rewindd.sh` downloads the prebuilt `rewindd-<arch>`
  from the plugin's GitHub Releases and **verifies its SHA-256** (against a
  committed `checksums/rewindd-<arch>.sha256` when present, else the checksum
  published in the same release) before installing — never an unverified binary.
  `triedBuild`/`triedDownload` prevent loops; each tier re-probes. If all fail,
  the bar says "recorder unavailable — install rust or check network" and
  query/wipe still work. So recording works from a clean `omarchy plugin add …
  --enable` on a machine with cargo *or* just network + curl. The release
  binaries are built by `.github/workflows/release.yml` (x86_64/aarch64 musl +
  sha256); this macOS authoring host does not produce or commit them.
- **Fallback operates on the real SQLite store.** `compat/rewindd.sh`
  query/timeline/moment/clips/stats/copy-clip/wipe use python3's `sqlite3` module
  against `rewind.db` (never a legacy JSONL index). A wipe deletes the real
  frame/ocr/clip/layout/event rows **and** unlinks the frame/crop files inside a
  transaction, then reports the true count; a missing DB reports 0 honestly. No
  `eval` of untrusted settings strings (load_state/merge_configure read only
  numeric/bool runtime fields via a safe read).
- **Quattro settings.** `state.json` persists only the runtime consent/arm
  boundary (`consentAt`, `armed`). Every customization value — byte cap,
  cadence, idle, excludes, title patterns, **and `armOnLogin`** — comes
  exclusively from the inline shell.json entry via `configure`, never
  duplicated to disk. `armOnLogin` is a user preference, so it is not persisted;
  startup auto-arm is deferred until the shell delivers `armOnLogin` (with
  recorded consent) via `configure` — never resumed from a stale on-disk bit.
- **Fail-closed pause.** If `hyprctl -j clients` errors, `evaluate_pause` returns
  the hard `Unknown` pause (never None/Idle) — a transient IPC failure while a
  private/excluded window is on screen cannot let capture proceed.
- **Synchronous overlay epoch.** Handling `set-pause reason=overlay` advances the
  privacy epoch synchronously, so an in-flight capture/clip job is invalidated
  the instant the overlay opens.
- **The byte cap counts ALL retained observation data.** The managed total is
  frame thumbnails + full-resolution OCR crops (`frames.crop_bytes`) + clipboard
  content + window layouts + OCR/search text — not just frame bytes. Pruning
  runs oldest-first on every growth path (capture AND clipboard commits), and
  advances by the oldest timestamp across every observation table, so a
  clipboard-, layout- or OCR-only period is bounded too, not only frame windows.
  WAL/page overhead is bounded separately by the deferred truncating checkpoint.
  OCR deletes a crop file first, then clears `crop_path`/`crop_bytes` only once
  the file is actually gone — so an interrupted deletion leaves the crop tracked
  for a later prune/wipe rather than orphaned; a `sweep_orphan_crops` pass in an
  authorized window reclaims any pre-existing unreferenced crop.
- **Cooperative shutdown.** On `shutdown` the daemon sets a `stopping` flag (the
  capture/OCR/clipboard workers check it and exit their loops), explicitly
  terminates the persistent `wl-paste` clipboard child (its own process group),
  and joins the workers; a 3 s watchdog guarantees the process still exits if a
  join hangs.
