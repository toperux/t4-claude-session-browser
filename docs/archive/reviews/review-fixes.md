# Review fixes — post-batch review of 9f502f7..47f9a91

> **Status (2026-09-13): landed in `c158a61`, released in 0.2.10.** Batch G is the whole
> plan. The R3 smoke and the accepted items (R6, R9, R10, R11) are tracked in
> `docs/backlog.md`.

Findings from the adversarial review of the seven review batches. Each row is
one change; `Decision` is filled in as we go.

| ID | Sev | Area | Finding | Conf | Decision |
|----|-----|------|---------|------|----------|
| R1 | high | tui | Project switch re-points cursor at old list's row 0 session, not the top | high | ✅ fix (a): `select_project` sets `session_sel = 0`, bypasses re-resolution |
| R2 | high | tui | Event drain ignores `quit`; type-ahead after `q` is executed as keys | high | ✅ fix: `while !app.quit && event::poll(..)` |
| R3 | med | gui | `marked.clear()` in `poll_delete` wipes marks made mid-delete and partial-failure survivors | high | ✅ fix: delete the line; `apply_index` retains alive ids |
| R4 | med | gui | Delete outcome overwritten by `warning_summary()` in `apply_index` | high | ✅ fix (a): set `status = summary` after `apply_index` |
| R5 | med | index/transcript | `transcript::load` pre-check needs compact `"type":"user"`; scanner tolerates whitespace; comment claims parity | high | ✅ fix (a): pre-check via `pub(crate) for_each_type`, drop `RECORD_*` |
| R6 | low | gui | In-transcript Find only searches capped (12k/20k) text | high | ✅ accept (a): note the limit in `cap_for_preview` doc comment |
| R7 | low | index | Dedupe comment wrong: transcripts are slug-scoped, only sidecars shared | high | ✅ fix: rewrite comment; keep dedupe |
| R8 | low | gui | "preview capped" note gated on `truncated`, which `cap_for_preview` never sets | high | ✅ fix: `cap_for_preview` returns bool, set `truncated` |
| R9 | low | index | Tightened `is_session_id` silently drops stems with space/non-ASCII from `discover` | med | ✅ accept (a) |
| R10 | low | installer | `yum` gets `--allow-downgrade` it does not know | med | ✅ accept (a) |
| R11 | low | installer | Desktop file `>` truncates before read; Exec quoting incomplete for `%` `"` `$` | med | ✅ accept (c) |
| R13 | low | gui | Long header status (duplicate-id warning carries two full paths) runs left over Reload/Sort/Search/title | high | ✅ fix (a): `Label::truncate()` + hover text |
| R12 | nit | gui/index | `truncate` allocates when nothing cut; `preview_lower` kept after `preview` dropped | high | ✅ fix (a): clear `preview_lower`/`preview_matches` with `preview`; leave `truncate` |

## Details

### R1 — TUI project switch cursor
`move_sel`/`jump` set `session_sel = 0` then call `refilter()`. `refilter`
captures the id at `visible[0]` of the *old* list and re-resolves it in the new
one, so the reset is dead and the cursor lands wherever the old project's
newest session sits in the new list. `g` in the same pane gives row 0; the two
paths disagree.

Options:
- (a) Always top on project switch (pre-batch behaviour). New `select_project`
  that sets `project_sel`, rebuilds `visible`, sets `session_sel = 0`,
  `sync_preview()`; bypasses `refilter`'s re-resolution.
- (b) Keep the highlighted session if it is in the new list, else top. Same
  function with `position(id).unwrap_or(0)`.

### R4 — delete status clobbered
`poll_delete` sets `status = summary` then `apply_index` sets
`status = warning_summary()` if the index has any warning. A single persistent
unreadable/duplicate file hides "deleted 2 of 5, then failed: …".

Options:
- (a) Delete summary wins: set `status` after `apply_index`. Warnings show on
  the next Reload.
- (b) Combine: `"{summary}; {warnings}"`.

### R5 — record-type pre-check
`RECORD_USER = "type":"user"` (compact). `for_each_type`/`skip_colon` in the
index scanner tolerate `"type" : "user"` and have a test for it. A non-compact
transcript indexes with the right title and counts but previews empty.

Options:
- (a) Make `for_each_type` `pub(crate)`; pre-check = any `type` value is
  `user`/`assistant`. Drop `RECORD_*` consts. Consistent with the scanner.
- (b) Keep compact check, fix the comment to say it is stricter than the
  scanner.

### R6 — Find over capped text
`preview_lower` is built from entries after `cap_for_preview`. Text past 12k
chars in a user/assistant/thinking message is unsearchable. Tool results are
unaffected (`searchable()` uses headline/preview).

Options:
- (a) Accept; note it in the `cap_for_preview` doc comment.
- (b) Build `preview_lower` in the worker before capping. Costs a lowercase
  copy of every uncapped user/assistant/thinking message.

### R9 — discover filter
`discover` reuses `is_session_id`; stems with a space or non-ASCII char are now
skipped with no warning. Claude Code writes UUIDs and `agent-*` ids only.

Options:
- (a) Accept.
- (b) Push a warning like the non-UTF-8 slug path.

### R10 — yum downgrade flag
`${pinned:+--allow-downgrade}` is passed to `yum` too. Real yum v3 has no such
flag; on every current host `yum` is dnf.

Options:
- (a) Accept.
- (b) Only pass the flag when `pm = dnf`.

### R11 — desktop file write
`{ grep …; printf …; } > csb.desktop` truncates the target before reading the
source; under `set -e` an unreadable source leaves an empty entry. Exec quoting
does not escape `%` `"` `$` `` ` `` `\` per the Desktop Entry spec.

Options:
- (a) Write to `csb.desktop.tmp` then `mv`. Skip the quoting.
- (b) Also escape the five spec characters in `$dir`.
- (c) Accept as is.

### R12 — nits
- `truncate()` returns `s.to_string()` when nothing is cut, so
  `cap_for_preview` copies every entry once per load (worker thread).
- `preview_lower`/`preview_matches` are not cleared when `preview = None`.

Options:
- (a) Clear `preview_lower`/`preview_matches` alongside `preview`; leave
  `truncate`.
- (b) Both.
- (c) Neither.

## Batches

One batch, one commit. All edits are small; `coder` implements, main session
reviews diff + gates.

| Batch | Items | Files | Tests |
|-------|-------|-------|-------|
| G | R1 R2 | `src/tui.rs` | R1: switching project lands on row 0 (extend existing tempdir-tree tests) |
| G | R3 R4 R8 R12 R6-comment | `src/gui.rs` | none new; `poll_delete` needs a live channel — covered by manual smoke below |
| G | R5 R7 | `src/index.rs` `src/transcript.rs` | R5: `load` keeps a record written as `"type" : "user"` |

Gates: `cargo fmt --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`,
`cargo test --workspace --locked`.

Manual smoke (GUI, human pass, not blocking the commit):
- delete a session in a tree with one duplicate id: status shows the delete
  summary, not "skipped 1 file";
- mark two sessions, delete one, mark a third while the bar says "deleting…":
  after it lands the third mark survives;
- preview an entry over 20k chars: "preview capped" note appears.

Accepted, no change: R9, R10, R11.

## Plan audit

Checked against the code before implementation.

- R2: the `while` guard alone still runs `app.sync_preview()` once after `q`,
  which can parse a large transcript before exit. Also skip that:
  `if app.quit { break }` before `sync_preview`, or guard the call.
- R4: `poll_delete`'s `Err` branch never calls `apply_index`, so "set after
  `apply_index`" means restructuring the match:
  `Ok(i) => { apply_index(i); status = summary }`,
  `Err(e) => status = format!("{summary}; reindex failed: {e}")`.
- R3: with `marked.clear()` gone, the reindex-failed branch leaves marks on
  ids that were just deleted. The index is equally stale there; Reload fixes
  both. Acceptable.
- R8: `Transcript::truncated` already means "entry count hit `max_entries`"
  (`transcript.rs:115`). Reusing it for per-entry caps is fine because the
  label says "preview capped" either way, but `csb show` and the TUI must not
  see the flag set by `cap_for_preview`: it is only set in the GUI worker,
  after `load`. Confirmed that is the only caller (`gui.rs:419`).
- R5: `for_each_type` walks every `"type"` in the line with no early exit. A
  tool result carrying embedded JSON can hold thousands; still a memmem sweep
  per line, well under the parse it replaces. No early exit needed. Delete
  the consts and the comment at `index.rs:359-365`; `transcript.rs` builds its
  own `Finder::new(br#""type""#)` once.
- R1: `select_project(sel)` is the only writer of `project_sel` besides
  `rebuild_projects` (reload/delete path, keeps `refilter`). Test goes through
  the existing `press` helper on the `tree()` fixture: move to Projects pane,
  step, assert `session_sel == 0` and `current()` is the newest session of
  the new list.
- R12: `preview = None` at two sites (`gui.rs:399` focus change, `:634` focus
  dropped). Clear `preview_lower`, `preview_matches` and reset
  `preview_matches_for` at both.

## GUI smoke (driven via screenshots + synthetic input, fixture tree)

- Delete in a tree with a duplicate id: status "moved 1 session(s) (136 B) to
  the recycle bin", not the warning. ✅ R4
- Confirm dialog: plans ready, live warning, Delete enabled, file gone after. ✅
- Preview of a 27k-char message: text ends in "…", "preview capped — run
  `csb show <id>`" note shown. ✅ R8
- Mark during in-flight delete (R3): not driven — trash + reindex on the
  fixture finish in milliseconds, no window to click in. Code path is the
  removed `marked.clear()` only.

### R13 — header status overlap (found during smoke)
`draw_header` puts `status` in a `right_to_left` layout as a plain `Label`.
Before the delete, the duplicate-id warning (two absolute paths) ran across
Reload, Sort, Search and the title. Pre-existing, but batch B's dedupe and
non-UTF-8 warnings made long statuses common.

Options:
- (a) `Label::new(..).truncate()` plus `on_hover_text(&self.status)`. Two lines.
- (b) Shorten the warning text itself (file names, not full paths).
- (c) Accept.
