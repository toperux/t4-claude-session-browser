# Codebase review — csb 0.2.9 (2026-09-12)

Full-repo review at commit 49f323d. Five parallel read-only passes (core index/transcript/paths, CLI + delete + update, GUI, TUI, packaging), each finding verified against the source with a concrete failure input; findings that could not be substantiated were dropped. Duplicates across passes are merged below. The "Ruled out" appendix lists what was checked and found fine, so it is not re-raised.

Severity: **P1** data loss / crash on plausible input / security · **P2** wrong behaviour on plausible input · **P3** perf or robustness on edge input · **P4** simplification / test gap.

**Decision** column is for us: `fix` / `defer` / `won't` / `?`.

## Decision table

| ID | Sev | Area | Finding | Conf | Decision |
|---|---|---|---|---|---|
| D1 | P1 | delete | `...` session id resolves to the parent dir on Windows → plan trashes the whole project dir, `session-env/`, `file-history/` | high | fix: allowlist `[A-Za-z0-9._-]+`, not all dots, no trailing `.`/space; non-matching stems silently skipped in `discover`; tests for `...`, `....`, `abc.`, `abc `, `a:b` |
| U1 | P2 | update | Self-update archive is never integrity-checked (`checksums` feature off) | high | fix: add `"checksums"` feature. Signatures deferred. |
| M1 | P3 | build | `rust-version = "1.82"` is stale: self_update rc.6 declares 1.88 (let-chains), sha2 0.11 needs 1.85 | high | fix: bump to 1.88, fix the comment |
| C1 | P2 | cli | `csb list` / `csb show` panic on closed stdout pipe | high | fix: one `BufWriter<StdoutLock>` + `writeln!` for list/show/delete plan output; map `BrokenPipe` → `Ok(())` in `main` (walk `err.chain()`, the io error is wrapped in context) |
| G1 | P2 | gui | "no sessions match" label is laid out below the clip rect, never visible | high | fix: `if visible.is_empty()` render label else scroll area; one message |
| G2 | P2 | gui | Preview collapse state is keyed by widget position, bleeds across sessions and find-filter | medium | fix: salt with `(session id, original entry index)`; per-session state persists on return |
| T1 | P2 | tui | Project/session selection is positional; reload/sort/delete silently re-points it | high | fix: capture slug + id before rebuild/refilter, re-find after; vanished project → All; keep index model, re-resolve around rebuilds. Interacts with T3: `reload_index` must force the preview reload |
| T2 | P2 | tui | Preview scrolls by logical entry, so most of a long message is unreachable | high | fix: split messages on `\n` into one `Line` each; keep pre-slice scrolling; widen `preview_scroll` to `usize` |
| K1 | P2 | packaging | .deb/.rpm depends omit dlopened libs (Xcursor, X11-xcb, EGL, wayland-egl, wayland-cursor) | high (verified) | fix: deb `+libxcursor1, libx11-xcb1, libegl1, libwayland-egl1, libwayland-cursor0`; rpm `+libXcursor, libX11-xcb, mesa-libEGL, libwayland-egl, libwayland-cursor`; comment names the crates; smoke = `debian:bookworm-slim`, `apt install ./csb.deb`, then `ldconfig -p` shows all five sonames (no display in a container, so not `csb gui`) |
| I1 | P3 | delete | Sessions keyed by bare id: duplicate stems across projects delete both; empty needle matches all | medium | fix: `bail!` on empty needle in `find`; dedupe ids in `Index::build`: keep the one with newest activity (deterministic), others to warning summary |
| I2 | P3 | delete | Non-UTF-8 project slug: delete reconstructs path from lossy slug (half-delete or undeletable); cache key collision; `--json` panics | high mech / low freq | fix: `discover` skips non-UTF-8 dirs + warning summary; `--json` emits `to_string_lossy`. `del::plan` via `meta.path` **dropped**: moot once discover skips |
| G3 | P3 | gui | plan + trash + reindex run synchronously on the egui thread, no spinner | high | fix: worker thread + channel (same shape as `focus`/`spawn_install`); Delete-click opens the dialog immediately with a spinner and Confirm disabled until plans arrive; Confirm runs execute + reindex on the worker; action-bar buttons disabled while pending |
| T3 | P3 | tui | Transcript reload on every filter keystroke / selection move, delete + reindex synchronous | high | fix: skip `load_preview` when selected id unchanged on filter/sort paths only; `reload_index` (`r`, post-delete) always forces it; drain queued input before loading. Delete-on-worker **deferred** |
| G4 | P3 | gui | Preview worker for a just-deleted session lands and clobbers the delete status | medium | fix: `pending = None` where the delete result is applied (post-G3 that is the worker-result handler) + drop results whose id ≠ `focused`; land with G3 |
| G5 | P3 | gui | Preview pane re-truncates and re-lays out every entry every frame | high | fix: memoize filtered list on `(id, needle)`; truncate in the GUI preview worker after `load` returns, **not** in `transcript::load` (shared with `csb show`, which prints full text). Virtualization (`show_viewport`) **deferred** |
| X1 | P3 | index | Four-prompt title budget consumed by records that clean to nothing → `(untitled …)` | medium | fix: clean inside `scan_file` before pushing; bump `CACHE_SCHEMA` |
| X2 | P3 | transcript | `transcript::load` fully deserializes every line incl. ~54% bookkeeping | high | fix: `memmem` pre-check for user/assistant before `from_slice`, sharing index.rs's byte-pattern constant so both scanners agree |
| X3 | P3 | transcript | Multi-MB tool results copied in full before 300-char truncation | high | fix: truncate (via existing char-safe helper) then `replace`; cap `headline_for` fallback |
| C2 | P3 | cli | Non-interactive stdin → `delete` prints `aborted`, exits 0 | high | fix: `bail!("aborted")`; on EOF say `no terminal; pass --yes` |
| C3 | P3 | cli | Mid-loop delete failure aborts without saying what was already trashed | high | fix: stop at first failure, report `deleted N of M, then failed`; **folded into S1** |
| T4 | P3 | tui | 80-col terminal clips size, msg count and `ACTIVE?` from session rows | high | fix: `Constraint::Min` on sessions/preview; move `ACTIVE?` to the front of the metadata line; drop indent + short timestamp under ~40 cols |
| T5 | P3 | tui | Marks are global across project/filter changes; `d` deletes off-screen sessions | medium | fix: keep global marks; confirm dialog shows "N of these are outside the current view" in **both** TUI and GUI |
| K2 | P3 | packaging | install.sh routes openSUSE into an rpm whose requires cannot resolve | medium | fix: drop the zypper branch, fall through to the tarball hint |
| K3 | P3 | packaging | `CSB_VERSION` pin to an older release fails under apt/dnf | medium | fix: apt `--allow-downgrades`, dnf `--allow-downgrade` when `CSB_VERSION` is set. dnf4 lacks the flag and errors loudly; Fedora ≥41 is dnf5. Loud beats today's silent no-op |
| K4 | P3 | packaging | curl-pipe scripts not wrapped in `main()`; truncated download runs the prefix | high | fix: `main() { … }; main "$@"` in both scripts |
| K5 | P3 | packaging | install.sh builds a local path from `SHA256SUMS` contents, unsanitised | medium | fix: `case "$file" in */*\|.*\|"") exit 1` after the awk (leading dot closes `..`) |
| K6 | P3 | packaging | `shasum --quiet` rejected by older macOS shasum | medium | fix: drop `--quiet` (note: `-s` is shasum-only, GNU has none) |
| K7 | P3 | packaging | `$dir` interpolated into a sed replacement unescaped (`&`, `|`, `\`) | high | fix: `grep -v '^Exec='` + `printf` the Exec line |
| K8 | P3 | packaging | No `StartupWMClass` / `app_id`, so the launcher icon never attaches to the window | medium | fix: `.with_app_id("csb")` on the eframe viewport |
| S1 | P4 | simplify | Delete-execute loop, plan summary and Sort label duplicated GUI ↔ TUI, already drifted | high | fix: `del::execute_all`, `del::PlanSummary`, `Sort::next/label`; all three front ends call them; **land before C/D** |
| S2 | P4 | simplify | `SessionMeta::first_ts` written and cached, never read | high | fix: delete field; schema bump shared with X1 |
| S3 | P4 | perf | `Cache::get` allocates a String per lookup; `BufRead::split` allocates a Vec per line | high | fix: `.as_ref()`; `read_until` + reused buf, done with X2 |
| S4 | P4 | perf | `Event::ToolUse.raw` unbounded and rendered whole in the GUI | medium | fix: cap at 20 000 like ToolResult, in the GUI preview worker (same place as G5), not in `blocks_of` |
| U2 | P4 | update | Windows update downloads and extracts the same zip twice | high | won't: `ponytail:` comment naming the cost |
| U3 | P4 | update | A failed check erases the previously-seen version for 24h | high | fix: overwrite `last_seen` only on success |
| K9 | P4 | packaging | `chmod 755 $tmp` for `_apt` but the .deb inherits umask | high | fix: `chmod 644` the .deb after download |
| K10 | P4 | packaging | Desktop-entry block guards on `csb.desktop` then copies `csb.png` unconditionally | high | fix: extend guard to `csb.png` |
| K11 | P4 | packaging | Version-resolution block duplicated verbatim in both install scripts | high | won't: cross-reference comment in each |
| Q1 | P4 | tests | `cli::delete` target selection and `del::plan` untested | high | fix: pure `select()` fn + tempdir tests; with batch B |
| Q2 | P4 | tests | `Cache` load/get/store/schema-reset/retain untested; `discover`, `find`, `filter` untested | high | fix: tempdir tests; with X1/S2 |
| Q3 | P4 | tests | gui: `selection_summary` ↔ `marked_or` agreement and `wsl_host_zoom` DPI parse untestable as written | high | fix: split `parse_applied_dpi`, test both |
| Q4 | P4 | tests | tui: no tests for `handle_key` / mode machine / selection identity across sort+reload | high | fix: synthetic `KeyEvent` tests over tempdir index; with batch C |

## Batches (decided 2026-09-12)

Order matters: A is independent and ships first; S1 next so C and D edit one copy of the delete loop; B, C, D, E can then go in any order; F last.

| Batch | Items | Notes |
|---|---|---|
| **A** | D1, U1, M1 | One-liners. Ship first. |
| **S** | S1 (+ C3 folded in) | Shared `execute_all` / `PlanSummary` / `Sort` helpers. Before C and D. |
| **B** | I1, I2, C1, C2, Q1 | Delete-path + CLI hardening, one PR. |
| **C** | T1, T2, T3, T4, T5 (TUI half), Q4 | TUI. |
| **D** | G1, G2, G3, G4, G5 (+ S4 folded in), T5 (GUI half), K8, Q3 | GUI. |
| **E** | K1, K2, K3, K4, K5, K6, K7, K9, K10, K11 | Packaging. Clean-container smoke for K1. |
| **F** | X1, X2, X3, S2, S3, U3, Q2 | Parse path + cache, one schema bump. |

Deferred, not scheduled: signatures on the updater (U1 caveat), TUI delete on a worker (T3), preview virtualization (G5), U2.

### Plan audit (2026-09-12)

Corrections folded into the table above:
- G5/S4 truncation moved out of `transcript::load` / `blocks_of` (shared with `csb show`) into the GUI preview worker.
- T1/T3 interaction: `reload_index` forces the preview reload; only filter/sort skip.
- K1 smoke uses `ldconfig -p` presence, not `csb gui` (no display in a container).
- G3 dialog behaviour while plans compute: spinner + disabled Confirm.
- I1 dedupe rule made deterministic: newest activity wins.
- C1: walk `err.chain()` for the BrokenPipe.
- X2: share index.rs's byte pattern.
- K3: dnf4 caveat recorded.
- K5: reject leading-dot names too.
- I2: `del::plan` via `meta.path` dropped as speculative once discover skips.

Verified during the audit: the digest gate is on the plain `update()` path (crate test `finish_update_rejects_a_mismatched_checksum_before_extracting`); `trash` 5.x initializes COM per call so a worker thread is fine; `ViewportBuilder::with_app_id` exists in egui 0.29; batch order S → B is required because C3 (S) and C2 (B) edit the same function.

---

## P1

### D1 · `...` session id resolves to the parent directory on Windows
- **Where:** `src/paths.rs:80-82` (`is_session_id`), `src/paths.rs:38-54` (`session_paths`), consumed by `src/del.rs:25-35`, `src/del.rs:38-66`
- **Category:** data-loss · **Confidence:** high (reproduced on this machine)
- **What:** `is_session_id` rejects only `.` and `..`. Win32 strips trailing dots from a path component, so `...`, `....` etc. resolve to the directory itself. `Path::file_stem("....jsonl")` is `"..."`, which passes the guard and is indexed as a session. `session_paths` then yields `projects/<slug>/...`, `session-env/...`, `file-history/...`, all of which canonicalize to existing dirs inside the claude root, so `ClaudeDir::contains` passes.
- **Failure scenario:** A file `%USERPROFILE%\.claude\projects\<slug>\....jsonl` exists (plain `Set-Content` creates it). `csb delete ... -y`, or ticking that row in GUI/TUI, moves the entire project dir, all of `session-env/` and all of `file-history/` to the recycle bin. The plan display shows the unresolved `...` forms, so nothing warns.
- **Fix:** In `is_session_id`, also reject ids that are all dots or end in `.`/space: `&& !id.chars().all(|c| c == '.') && !id.ends_with(['.', ' '])`. Add `"..."`, `"...."`, `"abc."` to the `session_paths_refuses_directory_walking_ids` test.

## P2

### U1 · Self-update archive is never integrity-checked
- **Where:** `Cargo.toml:34-41` (self_update features), `src/update.rs:44-76`, `src/update.rs:116-159`
- **Category:** security · **Confidence:** high
- **What:** self_update 1.0.0-rc.6 verifies GitHub's per-asset digest by default, but every digest path is `#[cfg(feature = "checksums")]` (crate `src/backends/common.rs:429-434`). This project uses `default-features = false` without `checksums`, so the block compiles out. The release publishes `SHA256SUMS` and both install scripts enforce it; the updater, which overwrites an already-trusted exe, does not.
- **Failure scenario:** Corrupt/truncated CDN object, or TLS interception with a trusted enterprise root, is extracted and moved over `csb.exe` / `csb-gui.exe` with no complaint. GUI daily banner makes it one click.
- **Fix:** Add `"checksums"` to the self_update feature list. No code change; `verify_release_digest` defaults on.
- **Audit (2026-09-12):** `checksums` = `dep:sha2` only. Gate at crate `update.rs:1320` hashes the tmp archive before extraction; mismatch is a hard error. GitHub publishes a digest for every v0.2.9 asset (checked via API). Caveats: (1) an *absent* digest is a silent skip, only a malformed one errors; forcing presence would need a custom verify callback, accepted as-is. (2) Integrity only: GitHub recomputes the digest on asset replacement, same as `SHA256SUMS`; authenticity would need the crate's `signatures` feature + a zipsign step in release.yml, **deferred**. (3) Pulls `sha2 0.11` / `digest 0.11` alongside the existing 0.10.x pair, small dup cost. (4) Surfaced M1.

### M1 · `rust-version = "1.82"` is stale
- **Where:** `Cargo.toml:6-7`
- **Category:** correctness · **Confidence:** high
- **What:** The comment justifies 1.82 with `Option::is_none_or`, but `self_update 1.0.0-rc.6` declares `rust-version = "1.88"` and uses let-chains, so cargo refuses to build this project on anything under 1.88 already. `sha2 0.11` (U1) needs 1.85. CI builds on `stable`, so nothing enforces the field.
- **Failure scenario:** Someone on 1.85 reads the field, expects a build, gets a resolver error naming self_update instead of a version message.
- **Fix:** Bump to `1.88` and rewrite the comment to name self_update as the floor.

### C1 · `csb list` / `csb show` panic on a closed stdout pipe
- **Where:** `src/cli.rs:40`, `src/cli.rs:45`, `src/cli.rs:50-67`, `src/cli.rs:105-123`
- **Category:** correctness · **Confidence:** high
- **What:** Header/table output uses `println!`, which panics on write error. Rust ignores SIGPIPE, so an early-exiting reader gives EPIPE → panic.
- **Failure scenario:** `csb list | head -5` on Linux/macOS → `thread 'main' panicked … Broken pipe`, exit 101 with backtrace. `csb show --raw <id> | head` exits 1 via `io::copy` instead.
- **Fix:** Write through one `BufWriter<StdoutLock>` with `writeln!` (as `show`'s body already does) and treat `ErrorKind::BrokenPipe` as `Ok(())` in `main`.

### G1 · "no sessions match" is never rendered
- **Where:** `src/gui.rs:1113-1116` (label), `src/gui.rs:1031-1033` (ScrollArea above it)
- **Category:** correctness · **Confidence:** high
- **What:** The label is emitted after a `ScrollArea` built with `auto_shrink([false, false])`, which claims all available height, so the label is clipped.
- **Failure scenario:** Search `zzzz` → pane shows `SESSIONS (0)` and blank space, looks hung.
- **Fix:** Render the label instead of the scroll area when `visible.is_empty()`.

### G2 · Preview collapse state keyed by widget position
- **Where:** `src/gui.rs:1454`, `src/gui.rs:1468`, `src/gui.rs:1487` (`.id_salt(ui.next_auto_id())`)
- **Category:** correctness · **Confidence:** medium
- **What:** `next_auto_id()` is a positional counter, so egui's persisted open/closed state belongs to the slot, not the entry.
- **Failure scenario:** Expand a tool_use block, type in Find → surviving entries shift slots and a different entry appears expanded. Switching to session B inherits A's expansion pattern.
- **Fix:** Carry the original entry index through the filter (`enumerate()` before `filter`) and salt with `(&meta.id, original_index)`.

### T1 · TUI selections are positional
- **Where:** `src/tui.rs:95` (`rebuild_projects`), `src/tui.rs:105` (`refilter`), `src/tui.rs:167` (`reload_index`); contrast `src/gui.rs:285`, `src/gui.rs:530` (GUI stores slug / id)
- **Category:** data-loss · **Confidence:** high
- **What:** `project_sel` / `session_sel` are only clamped, never re-resolved to the previously selected slug/id. `Index::projects()` orders by newest session and `filter` re-sorts, so reload, delete or sort cycle reorders the lists under a stale index.
- **Failure scenario:** Projects `[All, P1, P2, P3]`, select P2, `a d y` to prune it. Reload drops or reorders P2; row 2 is now P3 and the pane shows P3's sessions. User, believing they are still in P2, presses `a d y` and empties P3.
- **Fix:** Remember `selected_slug()` / `current().id` before `rebuild_projects` / `refilter`, re-find afterwards, fall back to 0 / All.

### T2 · Preview scrolls by logical entry
- **Where:** `src/tui.rs:539` (`skip(start)`), `src/tui.rs:163` (`max_scroll`), `src/tui.rs:560`, `src/tui.rs:570` (`Line::from(text.clone())`)
- **Category:** correctness · **Confidence:** high
- **What:** A whole message, newlines included, is one `ratatui::Line`; scrolling skips whole Lines. ratatui's wrapper treats `\n` as zero-width whitespace, so one entry is one scroll step regardless of rendered rows, and blank lines / code indentation are flattened.
- **Failure scenario:** 2000-char prompt in a 34-col × 20-row pane wraps to ~60 rows. Scroll 3 shows its first 20 rows; scroll 4 skips it entirely. Rows 21-60 are unreachable.
- **Fix:** Split entry text on `\n` into one `Line` per source line and use `Paragraph::scroll((preview_scroll, 0))` over the full preview.

### K1 · .deb/.rpm dependency lists omit dlopened libraries
- **Where:** `Cargo.toml:67` (deb `depends`), `Cargo.toml:79` (rpm `requires`); corroborated by `.github/workflows/release.yml:113-114` installing `libxcursor-dev libxrandr-dev libxi-dev`
- **Category:** correctness · **Confidence:** medium
- **What:** `$auto` only sees DT_NEEDED. winit's `x11-dl` dlopens `libXcursor.so.1`, `libXrandr.so.2`, `libXi.so.6`, `libX11-xcb.so.1`; x11rb via libloading dlopens `libxcb.so.1`; glutin dlopens `libEGL.so.1`, `libwayland-egl.so.1`. Only `libx11-6` of these is declared.
- **Failure scenario:** Minimal Debian + xrdp/Xvnc or a container: apt installs cleanly, `csb gui` aborts with `libXcursor.so.1: cannot open shared object file`.
- **Fix:** deb: add `libxcursor1, libx11-xcb1, libegl1, libwayland-egl1, libwayland-cursor0`. rpm: `libXcursor, libX11-xcb, mesa-libEGL, libwayland-egl, libwayland-cursor`. Rewrite the Cargo.toml comment to name the crates the list was derived from (winit x11 `Xlib`/`Xcursor`/`xcb` loaders, x11rb `dl-libxcb`, glutin EGL/GLX, wayland-sys, sctk, xkbcommon-dl) so a winit bump prompts a re-check. One manual `debian:bookworm-slim` + `apt install ./csb.deb` + `csb gui` smoke when the fix lands.
- **Audit (2026-09-12):** v0.2.9 Linux binary's DT_NEEDED is only libc/libm/libgcc_s, so `$auto` covers nothing GUI-side and the hand list is the whole contract. Verified dlopen sites in the locked crates: winit opens `Xlib`, `Xcursor`, `xcb` (= libX11-xcb) only; x11rb opens libxcb (transitively guaranteed by libx11-6, skip); glutin opens libEGL.so.1 before falling back to GLX; wayland-sys opens libwayland-client/egl/cursor. The original list's `libXrandr`/`libXi` were wrong: winit 0.30 does randr and xinput over xcb via x11rb. CI's `libxrandr-dev libxi-dev` install is historical.

## P3

### I1 · Sessions keyed by bare id string
- **Where:** `src/index.rs:216` (`marked_or`), `src/index.rs:232-246` (`find`), `src/gui.rs:350`, `src/gui.rs:533`, `src/tui.rs:612`, `src/cli.rs:171-176`
- **Category:** data-loss · **Confidence:** medium
- **What:** Deletion is keyed by `(project_slug, id)` but selection by `id` alone, and ids are not validated as UUIDs. Two project dirs can both hold `notes.jsonl`. Separately, `find` uses `starts_with(needle)` with no empty-needle guard, so `""` matches every session.
- **Failure scenario:** Mark `projA/notes`, Delete → `marked_or` returns both `notes` sessions, both trashed. `csb delete "" -y` with `$ID` unset deletes the only session on a one-session machine.
- **Fix:** Match on `(project_slug, id)` in `marked_or` and the UI lookups; `bail!("empty session id")` at the top of `find`.

### I2 · Non-UTF-8 project slug
- **Where:** `src/index.rs:271` (slug via `to_string_lossy`), `src/index.rs:392`, `src/paths.rs:38`, `src/del.rs:26`, `src/index.rs:665`, `src/index.rs:678` (cache key), `src/cli.rs:25-38` (`--json`)
- **Category:** data-loss · **Confidence:** high mechanism, low frequency
- **What:** `discover` keeps the real path in `SessionMeta::path` but `del::plan` reconstructs the transcript path from the lossy slug, which does not exist. Sidecar dirs (ASCII id) do exist. Cache is keyed on the lossy string, so two paths differing only in invalid bytes collide. `json!` on a `PathBuf` field unwraps a `to_value` that errors on non-UTF-8.
- **Failure scenario:** Linux, `projects/-home-u-caf\xe9/<uuid>.jsonl`: delete trashes `session-env/<uuid>` and `file-history/<uuid>`, reports success, transcript survives. With no sidecars, "nothing to delete" forever. `csb list --json` panics.
- **Fix:** `del::plan` uses `meta.path` for the transcript; key the cache on `path.as_os_str()` bytes or skip non-UTF-8 dirs in `discover` with a warning; emit `path.to_string_lossy()` in the JSON row.

### G3 · GUI delete path runs synchronously on the egui thread
- **Where:** `src/gui.rs:1294-1295` (`del::plan` per target), `src/gui.rs:577-597` (`run_delete`), `src/gui.rs:556-575` (`reindex`), `src/gui.rs:870-876` (Reload)
- **Category:** performance · **Confidence:** high
- **What:** Delete click runs `del::plan` → `dir_size` recursive walk per target; confirm runs `trash::delete_all` per session then a full `Index::build` (scan + cache rewrite), all inside `App::update` with no spinner. Preview and update paths already use workers.
- **Failure scenario:** "all" in a project with a few hundred sessions → window stops painting for seconds, Windows shows "Not Responding".
- **Fix:** Move plan + execute + reindex to a worker thread with a channel back (same pattern as `focus` / `spawn_install`), spinner in the action bar.

### T3 · TUI reloads the transcript on every keystroke, delete synchronous
- **Where:** `src/tui.rs:137` (`transcript::load` in `load_preview`), called from `src/tui.rs:110` (`refilter`) on every filter char / backspace / Esc / sort (`tui.rs:237,245,250,311`); `src/tui.rs:347` (`del::plan`), `src/tui.rs:263-276` (`execute` + `reload_index`)
- **Category:** performance · **Confidence:** high
- **What:** `refilter` unconditionally calls `load_preview`, which does a blocking full parse. Holding `j` queues repeats, each triggering a file read. Delete confirm freezes on the dialog with no feedback.
- **Failure scenario:** 20 MB session selected, type a 10-char filter → 200 MB re-read synchronously. `/authentication` → 14 full parses of whatever sits at the stale `session_sel`.
- **Fix:** Skip `load_preview` in `refilter` when the selected session id has not changed; drain pending input (`while event::poll(Duration::ZERO)`) before loading.

### G4 · Preview worker for a deleted session clobbers the delete status
- **Where:** `src/gui.rs:565-569` (`reindex` clears `focused`/`preview` but not `pending`), `src/gui.rs:368-395` (`poll_preview` accepts unconditionally)
- **Category:** correctness · **Confidence:** medium
- **Failure scenario:** Tick a large session, click its row (worker starts), Delete + confirm before it finishes. Status says "moved 1 session"; a frame later the worker's `File::open` fails and the status becomes `preview failed: … (os error 2)`. User thinks the delete failed.
- **Fix:** `self.pending = None` in the `reindex` branch that clears focus; drop received previews whose id ≠ `self.focused`.

### G5 · Preview pane re-lays out every entry every frame
- **Where:** `src/gui.rs:1219-1244` (`matches` rebuilt per frame, no virtualization), `src/gui.rs:1506` (`truncate(text, 12_000)` per entry per frame), `src/gui.rs:1466`, `src/gui.rs:1484`
- **Category:** performance · **Confidence:** high
- **What:** Unlike the sessions pane (`show_rows`), the preview draws all shown entries (up to 2000) each frame, allocating a String per entry, and re-runs `contains` over every lowercased entry when a find needle is set. The WSLg 150 ms heartbeat (`gui.rs:644-646`) keeps this running while idle.
- **Fix:** `show_rows` / `show_viewport` for on-screen entries only; cache the filtered index list keyed on the needle.

### X1 · Title budget spent on records that clean to nothing
- **Where:** `src/index.rs:378-389` (`prompts.len() < 4`), consumed at `src/index.rs:518`
- **Category:** correctness · **Confidence:** medium
- **What:** `scan_file` caps `prompts` at 4 non-meta user records; `clean_prompt` / `strip_noise_tags` / `CONTINUED` filtering only happens in `derive_title`. `<system-reminder>`-only records are not `isMeta` in real transcripts (verified in a local session) and burn slots.
- **Failure scenario:** Resumed session: compact summary + three reminder-only records fill the budget → title is `(continued session)` / `(untitled …)` though a real prompt is two lines down.
- **Fix:** Apply the cleaning inside `scan_file` before pushing.

### X2 · `transcript::load` deserializes every line
- **Where:** `src/transcript.rs:76`
- **Category:** performance · **Confidence:** high
- **What:** Every line becomes a `serde_json::Value` before `events_from` checks `type` and drops non-user/assistant. `index.rs` already pre-filters with `memmem`; transcript.rs does not reuse it. Local 3.1 MB session: 1374 lines, 634 useful.
- **Fix:** Byte check for `"type":"user"` / `"type":"assistant"` before `from_slice`.

### X3 · Tool results copied in full before truncation
- **Where:** `src/transcript.rs:159`, `src/transcript.rs:197`, `src/transcript.rs:201`
- **Category:** performance · **Confidence:** high
- **What:** `body.replace('\n', " ⏎ ")` copies the whole tool result, then truncates to 300 chars. `headline_for` `to_string()`s the entire tool input to keep 160 chars; fallback hits `NotebookEdit`.
- **Failure scenario:** 300 `Read` results × 30 KB → ~9 MB allocated and dropped per preview load; repeated per TUI keystroke (T3).
- **Fix:** Truncate first, then `replace` on the short slice.

### C2 · Non-interactive stdin makes `delete` a silent no-op
- **Where:** `src/cli.rs:231-234`, `src/cli.rs:257-263` (`confirm`)
- **Category:** correctness · **Confidence:** high
- **Failure scenario:** Cron runs `csb delete --older-than 30d` without `-y`, stdin closed. `read_line` returns 0, treated as "n": prints `aborted` to stdout, exits 0. Cleanup never happens and the job reports success.
- **Fix:** `bail!("aborted")` on the abort path, or detect EOF and error with "no terminal; pass --yes".

### C3 · Mid-loop delete failure hides what was already trashed
- **Where:** `src/cli.rs:236-239`; contrast `src/gui.rs:577-590`, `src/tui.rs:259-275`
- **Category:** data-loss · **Confidence:** high
- **Failure scenario:** 40 sessions selected, session 12's `session-env` is held open. Output: the plan, then `Error: deleting session <id-12>`. 1-11 are gone, 13-40 untouched, nothing says which.
- **Fix:** Mirror the UIs: count successes, print `deleted {ok} of {n}, then failed: {e}` before returning the error. (Folds into S1 if the loop is shared.)

### T4 · 80-column terminal clips the session row
- **Where:** `src/tui.rs:414` (22/33/45 % columns), `src/tui.rs:492` (metadata line)
- **Category:** correctness · **Confidence:** high
- **What:** Sessions pane gets 24 inner cells at 80 cols; the metadata line is ~46 cols (indent + 16-char timestamp + msgs + size + `ACTIVE?`). Everything after the date is cut, including the live-session warning the README advertises.
- **Fix:** `Constraint::Min` widths, or drop the indent and shorten the timestamp under ~40 cols.

### T5 · Marks are global across project/filter changes
- **Where:** `src/tui.rs:343` (`marked_or`), `src/tui.rs:336` (`a` marks visible), `src/index.rs:216`
- **Category:** data-loss · **Confidence:** medium
- **Failure scenario:** P1 → `a` (50 marked), P2 → `a` (30 more), `d`: dialog says "Move 80 session(s)" and lists 8 titles; `y` deletes 50 the user has not looked at since changing project. Only `A` clears all.
- **Fix:** Scope `a`/`d` to `visible` when a project or filter is active, or show "N of these are outside the current view" in the dialog. (GUI has the same mark model; check whether it has the same exposure.)

### K2 · install.sh routes openSUSE into an rpm that cannot resolve
- **Where:** `installer/install.sh:22-23` (zypper branch), `Cargo.toml:76-79`, `README.md:27`
- **Category:** correctness · **Confidence:** medium
- **Failure scenario:** Tumbleweed user runs the one-liner; it downloads, verifies, prompts for sudo, then zypper fails with "nothing provides mesa-libGL".
- **Fix:** Drop the zypper branch and fall through to the tarball message, or add openSUSE-compatible requires.

### K3 · `CSB_VERSION` pin to an older release fails under apt/dnf
- **Where:** `installer/install.sh:7` (docs), `installer/install.sh:66-67`
- **Category:** correctness · **Confidence:** medium
- **Failure scenario:** `CSB_VERSION=0.2.8 … | sh` on a 0.2.9 box: apt aborts "downgraded and -y was used without --allow-downgrades"; dnf silently no-ops.
- **Fix:** `--allow-downgrades` / `--allow-downgrade` when `CSB_VERSION` is set, or document the limitation.

### K4 · curl-pipe scripts not wrapped in `main()`
- **Where:** `installer/install.sh:8-71`, `installer/install-user.sh:9-77`
- **Category:** security · **Confidence:** high
- **What:** `sh` executes each complete command as it parses from the pipe; a truncated download runs the prefix. Verify-before-install ordering limits damage today but is load-bearing and undefended.
- **Fix:** Wrap each body in `main() { … }` with `main "$@"` as the final line.

### K5 · install.sh builds a local path from `SHA256SUMS` contents
- **Where:** `installer/install.sh:54` (`file=$(awk … SHA256SUMS …)`), used at `:58` (`curl -o "$tmp/$file"`) and `:66-67` (`sudo … "$tmp/$file"`)
- **Category:** security · **Confidence:** medium
- **Failure scenario:** Anything that can influence the release's SHA256SUMS names an entry `../../../../home/user/.bashrc.deb`; `curl -o` writes outside `$tmp` before the checksum check runs.
- **Fix:** `case "$file" in */*|"") echo "bad asset name" >&2; exit 1;; esac` after line 54.

### K6 · `shasum --quiet` rejected by older macOS
- **Where:** `installer/install-user.sh:46-47`
- **Category:** correctness · **Confidence:** medium
- **What:** `--quiet` arrived in Digest::SHA 6.00; macOS ≤ 11 ships older. Fails closed with `Unknown option: quiet`.
- **Fix:** Drop `--quiet` or use `-s`, accepted by both GNU and shasum.

### K7 · `$dir` interpolated into sed unescaped
- **Where:** `installer/install-user.sh:64`, `$dir` from `:12` (`CSB_INSTALL_DIR` or `$HOME`)
- **Category:** correctness · **Confidence:** high
- **Failure scenario:** `CSB_INSTALL_DIR=/opt/r&d/bin` → `Exec="/opt/rExec=csb d/bin/csb" gui`, a dead menu entry. A `|` in the path aborts sed after the binary is installed.
- **Fix:** `{ grep -v '^Exec=' "$tmp/csb.desktop"; printf 'Exec="%s/csb" gui\n' "$dir"; } > …`.

### K8 · Launcher icon never attaches to the window
- **Where:** `installer/csb.desktop:6` (`Icon=csb`, no `StartupWMClass`), `src/gui.rs:86-94` (title "Claude Session Browser", no `with_app_id`)
- **Category:** correctness · **Confidence:** medium
- **Failure scenario:** GNOME/KDE: launching from the menu opens a second dock entry with a placeholder icon.
- **Fix:** `StartupWMClass=Claude Session Browser` in the desktop file (installer-only), or `.with_app_id("csb")` in gui.rs.

## P4

### S1 · Delete loop, plan summary and Sort label duplicated GUI ↔ TUI
- **Where:** `src/gui.rs:556-597` vs `src/tui.rs:167-178`, `src/tui.rs:257-283`; `src/tui.rs:641-645` vs `src/gui.rs:1303-1306`; `src/tui.rs:25-39` vs `src/gui.rs:854-862`
- **Category:** simplification · **Confidence:** high
- **What:** Both run "rebuild index, drop dead marks, execute plans, count successes, summarise". Already drifted: GUI reports `moved N (1.2 MB)` and reindexes on error; TUI reports `deleted N` and breaks. GUI clears focus/preview, TUI clamps `session_sel`. Sort variants named `date` in one UI and `recent` in the other. G4 and T1 exist because the invalidation rule lives in two places.
- **Fix:** `del::execute_all(&ClaudeDir, &[DeletePlan]) -> (usize, Option<String>)`, `del::PlanSummary { bytes, files, live }`, `Sort::next()` / `Sort::label()` in index.rs. CLI (C3) then uses the same loop.

### S2 · `SessionMeta::first_ts` is dead
- **Where:** `src/index.rs:26`, `src/index.rs:330`, `src/index.rs:410`
- **Category:** simplification · **Confidence:** high
- **What:** Computed, stored, cached, never read anywhere (only definition, write, two test literals).
- **Fix:** Delete the field, bump `CACHE_SCHEMA`.

### S3 · Per-lookup and per-line allocations in the scanners
- **Where:** `src/index.rs:665` (`get(&path.to_string_lossy().into_owned())`), `src/index.rs:323`, `src/transcript.rs:71` (`split(b'\n')`)
- **Category:** performance · **Confidence:** high
- **What:** `HashMap<String,_>` accepts `&str` via `Borrow`; the owned String is wasted once per file in the rayon scan. `BufRead::split` yields an owned Vec per line.
- **Fix:** `get(path.to_string_lossy().as_ref())`; `read_until(b'\n', &mut buf)` with `buf.clear()`.

### S4 · `Event::ToolUse.raw` unbounded
- **Where:** `src/transcript.rs:151` (`pretty(&b["input"])`), consumed at `src/gui.rs:1470`; contrast `gui.rs:1490` (ToolResult truncated to 20 000)
- **Category:** performance · **Confidence:** medium
- **Failure scenario:** A `Write` carrying a 400 KB body: expanding that header lays out a 400 KB monospace label in one frame.
- **Fix:** Truncate where `ToolResult` already does.

### U2 · Windows update downloads the zip twice
- **Where:** `src/update.rs:126-156`
- **Category:** performance · **Confidence:** high
- **What:** One `Update` per entry in `BINARIES`, each with its own resolve → download → extract. Both Windows binaries are in the one zip. Double bytes, double progress bar, and a window where a mid-update release could mix versions.
- **Fix:** Probably leave it with a `ponytail:` note; or download once and `Move` both.

### U3 · Failed check erases the previously-seen version
- **Where:** `src/update.rs:186-199` (`check_now`), `src/update.rs:163-180` (`check_throttled`)
- **Category:** correctness · **Confidence:** high
- **Failure scenario:** Day 1 banner shows 0.3.0. Day 2 offline launch fails the check, writes `last_seen: None` + `last_check_secs: now`. Banner gone for 24 h even after reconnecting.
- **Fix:** Only overwrite `last_seen` from a successful result.

### K9 · `chmod 755 $tmp` for `_apt` but the .deb inherits umask
- **Where:** `installer/install.sh:46-48`, `installer/install.sh:58`
- **Category:** correctness · **Confidence:** high
- **What:** Under `umask 077` the .deb lands 0600 and apt still emits the `_apt` warning the comment set out to avoid. Cosmetic; apt falls back to root.
- **Fix:** `chmod 644 "$tmp/$file"` after download, or drop the chmod and comment.

### K10 · Desktop-entry block copies `csb.png` unconditionally
- **Where:** `installer/install-user.sh:61` (guard on `csb.desktop`), `:65` (`cp "$tmp/csb.png"`)
- **Category:** correctness · **Confidence:** high
- **Failure scenario:** A tarball with the entry but no icon → `cp: cannot stat` after the binary is already installed; `set -e` exits non-zero.
- **Fix:** Extend the guard to `[ -f "$tmp/csb.png" ]`.

### K11 · Version-resolution block duplicated in both scripts
- **Where:** `installer/install.sh:31-42`, `installer/install-user.sh:25-36`
- **Category:** simplification · **Confidence:** high
- **Fix:** Leave it; independently curl-piped scripts cannot share. Add a one-line cross-reference comment in each.

### Q1 · `cli::delete` selection and `del::plan` untested
- **Where:** `src/cli.rs:159-241` (only `parse_duration` is tested), `src/del.rs:25-35` (`plan`; tests hand-build `DeletePlan`)
- **What:** Nothing exercises id → target resolution and dedup, `--older-than` ∪ explicit ids, the `recent`/`--force` refusal, `--dry-run` stopping before execute, or the actual `plan → paths` mapping (the thing D1 breaks).
- **Fix:** Factor target selection into a pure `select(index, ids, older_than, project, now) -> Result<Vec<&SessionMeta>>`; tempdir tests for it and `del::plan`.

### Q2 · Cache and index discovery untested
- **Where:** `src/index.rs:631-692` (`Cache`), `Index::build`, `discover`, `find`, `filter`
- **What:** No test for `CACHE_SCHEMA` reset, the size/mtime hit predicate, the `retain(|_, s| !s.path.starts_with(projects))` cross-tree rule, `discover`'s depth-1 / `.jsonl` / `is_session_id` rule, or `find` ambiguity.
- **Fix:** Tempdir `store → load → get` test (hit, size change, mtime change, schema bump, other-tree retention); one `discover` test asserting `projects/<slug>/<id>/` and `projects/<slug>/memory/` are skipped.

### Q3 · gui: two load-bearing invariants untested
- **Where:** `src/gui.rs:539-554` (`selection_summary`, comment says "must mirror `Index::marked_or` exactly"), `src/gui.rs:140-183` (`wsl_host_zoom`, DPI parse inline with the `reg.exe` spawn)
- **Fix:** Split out `parse_applied_dpi(&str) -> Option<f32>`; test it plus `selection_summary().0 == marked_or(..).len()` over marked / empty / no-focus.

### Q4 · tui: no tests for the state machine
- **Where:** `src/tui.rs` (no `#[cfg(test)]`)
- **What:** `handle_key`, `move_sel`, `jump`, `refilter` clamp and the Browse/Filter/Confirm modes are pure over `App` and reachable with a tempdir index. T1 would have been caught by "same session id stays selected across `s` / `r`".
- **Fix:** `#[cfg(test)]` module driving `handle_key` with synthetic `KeyEvent`s.

---

## Ruled out (checked, not an issue)

**Core**
- `for_each_type`'s "`"type"` cannot occur inside a JSON string" holds; 0 nested hits across 193 local transcripts.
- First `"timestamp"` on a line is the record's own in every sampled dual-timestamp line.
- No mid-codepoint slicing: `short_id`, `truncate`, `tag_value`, `strip_tag_blocks` all derive indices from `char_indices` / `find`; tested for non-ASCII.
- `ClaudeDir::contains` cannot be escaped: both sides canonicalized, `starts_with` is component-wise, `\\?\` strip is symmetric and leaves `UNC\`.
- `..jsonl` → stem `.` is rejected; `.`, `..`, `""`, `a/b`, `a\b` are tested. (`...` is not; see D1.)
- CRLF transcripts parse; trailing `\r` is JSON whitespace.
- `Index::filter` cloning is per state change, not per frame.
- Cache size+mtime collisions are not a staleness vector: JSONL only grows by append.
- `is_recent` maxes record timestamp with mtime, so a bad `last_ts` still trips the guard.

**CLI / update**
- `if let Some(Cmd::Update{..}) = args.cmd` then `match args.cmd`: binds only `bool`, no move.
- Symlinked transcript to `/etc/passwd` is refused by `contains`; `execute` validates every path before trashing any.
- Cache hit requires both size and mtime to match a fresh `metadata()` call.
- Failed update never leaves a truncated binary: `Move::to_dest` renames (EXDEV copy-then-rename fallback); `self_replace` covers the running exe; Windows in-use gives the documented "close it, then `--force`".
- `parse_duration` edge cases (`-5d`, `""`, `7y`, overflow) are errors, tested.
- `history.jsonl` and sessions-index are never in a plan; `session_paths` builds only four `<id>`-scoped paths.
- `-p` ignored for explicit ids is documented in the flag help.
- `CSB_NO_UPDATE_CHECK=0` does not disable checks.

**GUI**
- No OOB after delete/filter: `visible` only assigned in `refilter` which nulls `anchor`; click/shift-range consumed same frame; `reindex` retains live marks.
- Confirm dialog deletes exactly the frozen plans; `modal-veil` Area wins hit test and `ui_contains_pointer`.
- No slicing in gui.rs; no bare `unwrap`/`expect`.
- `focus` replaces `pending` wholesale, so an old scan cannot overwrite a newer one; every worker calls `request_repaint` before exit.
- `remove_var("WAYLAND_DISPLAY")` runs single-threaded, before the zoom thread and `run_native`.
- build.rs: `res.compile()` failure is a warning; version bump forces rebuild so VERSIONINFO cannot go stale.
- `MIN_WINDOW` in physical pixels is a documented tradeoff; egui clips, no panic.

**TUI**
- Terminal restore runs on every exit path incl. `Terminal::new` failure; panic hook chains the previous hook.
- `ListState` `Some(0)` over zero items is safe in ratatui 0.29; `saturating_sub`/`clamp` throughout; `current()` returns `Option`.
- Windows: filters `KeyEventKind::Press`; crossterm 0.28 returns `None` for bare modifier keys, so shift-Y does not cancel the dialog.
- `event::read()` blocks on `/dev/tty`, no spin; `Terminal::draw` autoresizes.
- Display-width miscount (CJK/emoji) only clips instead of ellipsizing; `set_stringn` drops whole graphemes.
- `mem::replace(mode, Browse)` guarantees the dialog closes on any key; the captured plans match what `y` deletes.
- Ctrl+D opens the dialog (modifier fall-through); harmless, still needs `y`.
- `centered()` cannot underflow.

**Packaging**
- Asset-name contract holds end to end: only the Windows zip carries the target triple, `self_update` filters on it; README, notes table and scripts agree.
- Both scripts verify before use, in `( … )` subshells that `set -e` propagates from.
- `grep … | sha256sum -c` on an empty match still fails (exit 1).
- `~/.claude` is never touched by any install or uninstall path; Inno removes only `{app}` and its PATH entry.
- Inno runs `PrivilegesRequired=lowest`, `{localappdata}`, HKCU PATH only.
- PATH add/remove is symmetric and safe on empty/absent `Path`; `%VAR%` entries preserved.
- `package_managed()` tests `/usr/bin`, exactly where deb/rpm install; CLI and GUI both surface the hint.
- `Exec=csb gui` is a real subcommand; the sed pattern matches the shipped line.
- No BSD-userland traps in install-user.sh; install.sh is gated to Linux x86_64 before touching `sha256sum`.
- Tarball is flat; updater's default `bin_path_in_archive` + `EXE_SUFFIX` finds the binary.
