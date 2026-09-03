# CI/CD alignment plan — t4-claude-session-browser (csb)

## Context

This repo is one of three t4 projects whose GitHub Actions workflows are being aligned to a
single shared shape. The other two — `t4-markdown-viewer` (Tauri 2, no npm) and `t4-git-ui`
(Tauri 2 + npm, cargo workspace) — are out of scope here and have their own copies of this
plan. Everything below is self-contained; the shared shape is authoritative, and where this
repo already matches it, the item says "keep".

csb is pure Rust (egui/eframe), ships a CLI plus a GUI binary, and self-updates via
`self_update`, which matches release assets by target triple. That constraint is why the
release workflow builds per-triple archives and publishes one aggregate `SHA256SUMS` — those
stay exactly as they are.

Principles for every edit:

1. **Surgical.** Every changed line traces to a checklist item below. No drive-by rewrites of
   the release-notes body, comments, or unrelated steps.
2. **Don't touch what the updater depends on.** Asset names, archive layout, `SHA256SUMS`.
3. If this plan turns out to be wrong about something (a path, a runner name, an action
   version), fix the plan file in the same commit so it stays truthful.

## Decisions (resolved 2026-09-03, shared across all three repos)

| # | Decision | Resolution | Applies here |
| --- | --- | --- | --- |
| D1 | `paths-ignore` docs/md on CI | **Adopt in all three.** No `.md` test fixtures anywhere; no required-status-check rules to hang. | yes |
| D2 | Which leg runs `cargo fmt --check` | **Linux leg only.** Same code on every leg, so run it once on the cheapest runner. | yes — moves from Windows |
| D3 | git-ui asset naming | `T4-Git-UI_<ver>_<suffix>`, mirroring mdv. | no |
| D4 | git-ui updater | No. | no |
| D5 | Action pinning | **Majors + dependabot.** Verify current majors first (see Risks). | yes |
| D6 | Universal macOS for git-ui | Universal. csb stays per-triple (`self_update`). | no — keep per-triple |
| D7 | Tests inside release builds | **None.** CI already tests the commit. | yes — already none; nothing to add |
| D8 | `--locked` on release builds | **Release builds only.** | yes — already present; keep |
| D9 | Drop `"version"` from `tauri.conf.json` | Yes, own commit per repo. | no — not a Tauri app |
| — | Windows bundle (Tauri repos) | NSIS only, no `.msi`. | no — csb uses Inno Setup; keep |

## 1. Target `.github/workflows/ci.yml` (complete file)

Replace the file with this. Changes from today: `paths-ignore` (D1), concurrency group name,
`checkout@v7`, Format step gated on Linux instead of Windows (D2), `--workspace` on clippy and
test. Everything else — Linux deps, matrix, comments — is preserved.

(This section said `checkout@v5` when it was written; v7 is the current major — see Risks.)

```yaml
name: CI

on:
  push:
    branches: [main]
    # A docs-only push has nothing here to check. Mixed commits still run.
    paths-ignore:
      - "docs/**"
      - "**/*.md"
  workflow_dispatch:

env:
  CARGO_TERM_COLOR: always

# One run per ref: pushing again while CI is still going cancels the older run
# rather than queueing a second 3-OS matrix nobody is waiting for.
concurrency:
  group: ${{ github.workflow }}-${{ github.ref }}
  cancel-in-progress: true

jobs:
  check:
    name: ${{ matrix.os }}
    runs-on: ${{ matrix.os }}
    strategy:
      fail-fast: false
      matrix:
        # 22.04 rather than latest: the release binary links against the
        # builder's glibc, so building on the oldest supported runner is what
        # makes the tarball run on more than just the newest distros. CI matches
        # the release matrix so a break shows up here first.
        os: [windows-latest, macos-latest, ubuntu-22.04]

    steps:
      - uses: actions/checkout@v7

      # eframe with the glow/x11/wayland features. rustls keeps OpenSSL out of
      # it, so there is deliberately no libssl-dev here.
      - name: Linux build dependencies
        if: runner.os == 'Linux'
        run: |
          sudo apt-get update
          sudo apt-get install -y pkg-config libx11-dev libxcursor-dev \
            libxrandr-dev libxi-dev libxkbcommon-dev libwayland-dev libgl1-mesa-dev

      - uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy, rustfmt

      - uses: Swatinem/rust-cache@v2

      # Formatting is platform-independent, so check it once, on the cheapest runner.
      - name: Format
        if: runner.os == 'Linux'
        run: cargo fmt --check

      # `-D warnings` goes after `--`, not in RUSTFLAGS: as an env var it also
      # applies to every dependency, so one warning in a crate we do not own
      # turns the build red.
      - name: Clippy
        run: cargo clippy --workspace --all-targets -- -D warnings

      - name: Test
        run: cargo test --workspace
```

## 2. `.github/workflows/release.yml` changes

The release workflow already has the shared `version` → `build` → `publish` shape. Only:

- [x] `actions/checkout@v4` → `@v7` in **both** the `version` and `build` jobs (two occurrences).
      The plan said `@v5`; v7 is the current major.
- [x] Nothing else. Specifically **do not** add a test step (D7), and keep `--locked` (D8),
      the per-triple matrix, the Inno Setup installer, `cargo-deb`/`cargo-generate-rpm`,
      `SHA256SUMS`, and the release-notes body untouched.
- [x] `actions/upload-artifact` / `actions/download-artifact`: pin to the current major (see
      Risks). Bumped to `upload-artifact@v7` and `download-artifact@v8`.

## 3. Non-workflow changes

- [x] **`.github/dependabot.yml`** (new, optional, own commit) — keep the action majors from
      drifting again:

  ```yaml
  version: 2
  updates:
    - package-ecosystem: github-actions
      directory: /
      schedule:
        interval: monthly
    - package-ecosystem: cargo
      directory: /
      schedule:
        interval: monthly
  ```

- [ ] **Release skill** (optional follow-up, not required for this alignment) — this repo has
      no `.claude/skills/release/SKILL.md`. If one is wanted, the version lives in two files
      here: `Cargo.toml` and `Cargo.lock` (refresh the lock with `cargo check`, never by
      hand). The `version` job in `release.yml` compares the tag against `Cargo.toml` and
      rejects anything that is not plain `x.y.z`.

## 4. Risks to verify

- **`ubuntu-22.04` runner lifetime — CONFIRMED RETIRING; unresolved, needs a cross-repo
  decision.** Both workflows build Linux on it so the tarball, `.deb` and `.rpm` link against
  an old glibc. GitHub (`actions/runner-images#14254`) has the label deprecated from
  **2026-09-17** and fully unsupported on **2027-04-17**, with brownouts — jobs on the label
  simply fail — on **2027-03-23, 2027-03-30, 2027-04-06 and 2027-04-13, each 14:00–00:00
  UTC**. Until then it keeps working, with longer queue times near peak.

  Per this plan's own instruction the label was **left alone** in both workflows rather than
  silently switched to `ubuntu-latest`, which would raise the glibc floor from 2.35 to
  whatever the newest image carries and quietly drop older distros for everyone who installs
  the tarball or the packages. The fix — bump to `ubuntu-24.04`, keep 22.04 via
  `container: ubuntu:22.04`, or `cargo-zigbuild` — has to be chosen once and applied to all
  three repos. Nothing is urgent before March's brownouts. `t4-markdown-viewer` records the
  same finding, so the decision is still open there too.
- **Action majors — resolved 2026-09-03.** The guesses in this plan were stale. Queried the
  releases API: the current majors are `checkout@v7` (v7.0.1), `upload-artifact@v7` (v7.0.1)
  and `download-artifact@v8` (v8.0.1), and all three are pinned at the major. Recorded as
  asked: `upload-artifact@v7`, `download-artifact@v8`.

  upload v7 and download v8 are the matched pair, and the release pipeline's behaviour is
  unchanged: v7's unzipped single-file upload is opt-in via `archive: false`, which this
  workflow does not set, so the `packages-*` artifacts are still zipped; v8 decides whether
  to unzip from the content type and still accepts `path` and `merge-multiple`, which the
  `publish` job relies on before it computes `SHA256SUMS`. v8 also fails a download on a
  digest mismatch instead of warning. `checkout@v7`'s breaking change is a safer
  `pull_request_target` default; neither workflow uses that trigger.
- **Dependabot with no PR trigger — found in review, fixed here, still open in the other two
  repos.** Section 1's trigger list came from the shared shape, which predates the decision to
  add dependabot and so never accounted for it. Dependabot pushes to `dependabot/**` and opens
  a PR; a `push: branches: [main]` trigger matches neither, so every bump would have arrived
  with no checks at all. With `Cargo.lock` tracked and the release job building `--locked`, a
  bump that fails `clippy -D warnings` would have been found only after the merge to `main`,
  and a bad lockfile could reach a tagged release. `ci.yml` here now also triggers on
  `pull_request` against `main`, with the same `paths-ignore`. That is a deliberate
  divergence from the shared shape and it is cheap — everyday work is pushed straight to
  `main`, so in practice this fires only for dependabot. `t4-markdown-viewer` and `t4-git-ui`
  both added dependabot with the same main-only trigger and have the same hole; the shared
  shape should take this change too.
- **Stale major left behind on purpose.** `softprops/action-gh-release` is at v3; the publish
  job still pins `@v2`. Section 2 says to touch nothing else in `release.yml`, and this action
  is the one step that cannot be rehearsed without cutting a real release, so it was left for
  dependabot to propose with a diff a human can read. `taiki-e/install-action@v2` and
  `Swatinem/rust-cache@v2` are already their current majors.
- **`--workspace` on a single-package manifest — resolved.** csb has no `[workspace]` table.
  Ran `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace`
  locally on Windows: both pass, 38 tests green, so cargo treats the lone package as a
  one-member workspace as expected. `cargo fmt --check` also passes, which is what the Linux
  leg will now run.

## 5. Commit plan

In this order, each its own commit:

1. `docs: add CI alignment plan` — this file. (May be squashed into commit 2.)
2. `ci: align workflow with the other t4 projects` — section 1.
3. `release: align workflow with the other t4 projects` — section 2.
4. `ci: add dependabot` — section 3, if doing it.

Commits 2 and 3 are safe to land back-to-back. Do not tag a release as part of this work.

## 6. Verification

Sections 1–3 are committed on `main` (locally, unpushed) as of 2026-09-03, in the order
above. The local checks are folded into Risks. Everything below needs a push, so it is all
still outstanding.

Note the trigger when planning this: CI fires on a push to `main`, on a pull request
targeting `main`, and on `workflow_dispatch`. Pushing this work to a side branch on its own
runs **nothing** — the original wording of this checklist assumed otherwise and was wrong.
`workflow_dispatch` ignores `paths-ignore`, so it is the way to exercise a branch.

- [ ] Push the work to a branch and open a PR against `main`. CI runs; all three legs green.
      (Or dispatch `CI` on the branch, which also runs all three legs but proves nothing
      about `paths-ignore`.)
- [ ] In that run, `Format` executed on `ubuntu-22.04` only and was skipped on the other two.
- [ ] Add a commit to the PR touching only `docs/plans/ci-alignment.md`. CI does **not** run
      for it.
- [ ] Trigger `Release` via `workflow_dispatch` on the branch. All four `build` legs green;
      `publish` skipped (it is gated on a tag ref). Artifacts `packages-*` contain the same
      files as before this change.
- [ ] Merge to `main`. CI green on `main`.
- [ ] After the first dependabot PR appears, confirm it shows the three CI legs.
