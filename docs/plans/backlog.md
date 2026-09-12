# Backlog

Open items that no plan file schedules. Each row points at where the decision
was made. Remove a row when it lands or is dropped.

| Item | Source | Trigger / when |
|------|--------|----------------|
| K1 clean-container smoke: install the .deb in a bare `debian:bookworm` container, check `ldconfig -p` covers every library the GUI dlopens | codebase-review.md K1 | next release, needs the built .deb |
| shellcheck pass on `installer/install.sh` and `installer/install-user.sh`; only `sh -n` has run | codebase-review.md batch E | when shellcheck is on hand (CI job or local install) |
| R3 GUI smoke: marks made during an in-flight delete survive | review-fixes.md R3 | needs a delete slow enough to click during; a real tree with a large sidecar dir |
| Updater signatures: `self_update` `signatures` feature + zipsign step in release.yml. `checksums` gives integrity only, and an absent GitHub digest is a silent skip | codebase-review.md U1 audit | if the release pipeline is ever considered untrusted |
| TUI delete on a worker thread; today `execute_all` + reindex block the draw loop | codebase-review.md T3 | if a delete on a real tree visibly freezes the TUI |
| GUI preview virtualization (`ScrollArea::show_viewport`) | codebase-review.md G5 | if a capped preview still lays out too slowly |
| Windows update downloads and extracts the zip twice | codebase-review.md U2 (won't, `ponytail:` comment) | only if update time on Windows becomes a complaint |
| Version-resolution block duplicated in both install scripts | codebase-review.md K11 (won't, cross-ref comment) | if a third script appears |
| `discover` silently skips stems with a space or non-ASCII char | review-fixes.md R9 (accept) | if a user reports a missing session |
| `yum` gets `--allow-downgrade` it does not know | review-fixes.md R10 (accept) | if a RHEL 7 host matters |
| Desktop-file write truncates before read; Exec quoting incomplete for `%` `"` `$` | review-fixes.md R11 (accept) | if a custom install dir with those chars is reported |
| GUI Find only searches capped preview text (12k/20k chars per entry) | review-fixes.md R6 (accept) | if a search miss on a long message is reported |
