# Backlog

Open items that no plan file schedules. Each row points at where the decision
was made. Remove a row when it lands or is dropped. Last reconciled against the
repo and CI on 2026-09-13, after the 0.2.10 release.

| Item | Source | Trigger / when |
|------|--------|----------------|
| `ubuntu-22.04` runner retirement: deprecated from 2026-09-17, brownouts 2027-03-23 / -03-30 / -04-06 / -04-13 (14:00–00:00 UTC), unsupported 2027-04-17 (`actions/runner-images#14254`). Both `checks.yml` and `release.yml` build Linux on it for the glibc 2.35 floor. Options: `ubuntu-24.04`, keep 22.04 via `container: ubuntu:22.04`, or `cargo-zigbuild`. Decide once for all three t4 repos | archive/ci/ci-alignment.md §4, round-2 §5 | before the first brownout on 2027-03-23; expect longer queues from 2026-09-17 |
| Dependabot PR #2 (grouped cargo bump: crossterm, dirs, eframe, egui, ratatui, self_update, trash) fails all three CI legs: the egui major moved `TopBottomPanel` / `SidePanel` and changed `eframe::App`. A hand migration, not a merge | `gh pr view 2` | when bumping egui; until then either leave it red or comment `@dependabot ignore this major version` on the egui and eframe entries so the other five can go green |
| K1 clean-container smoke: install the .deb in a bare `debian:bookworm-slim` container, check `ldconfig -p` covers every library the GUI dlopens. No record of it running before 0.2.10 shipped the fix | archive/reviews/codebase-review.md K1 | now, against the 0.2.10 .deb |
| shellcheck pass on `installer/install.sh` and `installer/install-user.sh`; only `sh -n` has run | archive/reviews/codebase-review.md batch E | when shellcheck is on hand (CI job or local install) |
| R3 GUI smoke: marks made during an in-flight delete survive | archive/reviews/review-fixes.md R3 | needs a delete slow enough to click during; a real tree with a large sidecar dir |
| Updater signatures: `self_update` `signatures` feature + zipsign step in release.yml. `checksums` gives integrity only, and an absent GitHub digest is a silent skip | archive/reviews/codebase-review.md U1 audit | if the release pipeline is ever considered untrusted |
| TUI delete on a worker thread; today `execute_all` + reindex block the draw loop | archive/reviews/codebase-review.md T3 | if a delete on a real tree visibly freezes the TUI |
| GUI preview virtualization (`ScrollArea::show_viewport`) | archive/reviews/codebase-review.md G5 | if a capped preview still lays out too slowly |
| Windows update downloads and extracts the zip twice | archive/reviews/codebase-review.md U2 (won't, `ponytail:` comment) | only if update time on Windows becomes a complaint |
| Version-resolution block duplicated in both install scripts | archive/reviews/codebase-review.md K11 (won't, cross-ref comment) | if a third script appears |
| `discover` silently skips stems with a space or non-ASCII char | archive/reviews/review-fixes.md R9 (accept) | if a user reports a missing session |
| `yum` gets `--allow-downgrade` it does not know | archive/reviews/review-fixes.md R10 (accept) | if a RHEL 7 host matters |
| Desktop-file write truncates before read; Exec quoting incomplete for `%` `"` `$` | archive/reviews/review-fixes.md R11 (accept) | if a custom install dir with those chars is reported |
| GUI Find only searches capped preview text (12k/20k chars per entry) | archive/reviews/review-fixes.md R6 (accept) | if a search miss on a long message is reported |
