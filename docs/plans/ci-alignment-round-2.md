# CI/CD alignment, round 2 — t4-claude-session-browser (csb)

## Context

This repo is one of three t4 projects (with `t4-markdown-viewer` and `t4-git-ui`) whose
GitHub Actions workflows are kept to a single shared shape. Round 1 landed here in commits
`02eb73c` and `6c3dd03`; `docs/plans/ci-alignment.md` is its record and stays as-is.

Round 1 worked, but each of the three repos was executed by someone who could not see the
other two, and all three independently patched the same gap — Dependabot PRs arriving with no
checks — in three incompatible ways. Round 2 removes that drift. Everything here is decided;
nothing needs re-litigating.

**csb-specific:** pure Rust (egui/eframe), CLI + GUI binary, self-updates via `self_update`,
which matches release assets by target triple. The per-triple archives, the Inno Setup
installer, and the aggregate `SHA256SUMS` are all forced by that and are **not** touched.

Principles:

1. **Surgical.** Every changed line traces to a checklist item below.
2. **Don't touch what the updater depends on.** Asset names, archive layout, `SHA256SUMS`.
3. **Do not invent improvements.** If you find something worth changing that is not in this
   plan, do not apply it — write it under *Deviations and findings* at the bottom. Round 1
   drifted precisely because good local judgment was applied in three places at once. A
   finding recorded there gets picked up by the next master pass and applied to all three.

## Round 2 decisions (shared across all three repos)

| # | Decision | Resolution | Applies here |
| --- | --- | --- | --- |
| D10 | Dependabot | **Drop it everywhere.** Its only output is PRs; no repo runs anything on `pull_request`, so bumps arrive unchecked. What matters is **security alerts** — a repo setting, no config file, no PRs, no Actions minutes. Action majors don't rot silently: GitHub annotates runs on retiring runtimes. | yes — delete `dependabot.yml` **and** the `pull_request` trigger added for it |
| D11 | `--locked` in CI | **All three.** `clippy … --locked` and `test --locked`. Supersedes round 1's D8 "release only": a stale `Cargo.lock` otherwise goes green on the push and kills every release leg after the tag is public. | yes — add |
| D12 | Gating a release on the checks | **A reusable `checks.yml` (`on: workflow_call`), called by both `ci.yml` and `release.yml`, gating `publish`.** A `./` path call runs at the caller's commit, so a release checks the tagged tree. Supersedes mdv's `gh run watch` gate. | yes — new file, new job |
| D13 | Semver regex in the `version` job | **Tag refs only**, so `workflow_dispatch` can build mid-bump. | yes — move it |
| D14 | Action majors | `checkout@v7`, `setup-node@v7`, `upload-artifact@v7`, `download-artifact@v8`. `action-gh-release` stays `@v2` in all three deliberately. | **nothing to bump.** Round 1 already put this repo on `checkout@v7`, `upload-artifact@v7`, `download-artifact@v8`; it uses no `setup-node`. Carry the versions into `checks.yml` unchanged. |

Round 1 decisions D1–D9 still hold and are already implemented here. D8 is superseded by D11.

## 1. New file — `.github/workflows/checks.yml`

The check matrix moves here verbatim from `ci.yml`, with `--locked` added (D11). This becomes
the only place the matrix is defined.

```yaml
name: Checks

# Called by ci.yml on a push and by release.yml on a tag. Referenced by a local
# `./` path, so it always runs at the caller's commit - which is what lets a
# release verify the exact tree it is about to publish.
on:
  workflow_call:

env:
  CARGO_TERM_COLOR: always

jobs:
  check:
    name: ${{ matrix.os }}
    # A called workflow runs with the caller's token, and release.yml grants
    # `contents: write` for `publish`. Without this the check matrix would
    # inherit write access it has no use for; it only builds and tests.
    permissions:
      contents: read
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
      #
      # `--locked` because the release build passes it too: without it here a
      # stale `Cargo.lock` goes green, and then kills every release leg after
      # the tag is already public.
      - name: Clippy
        run: cargo clippy --workspace --all-targets --locked -- -D warnings

      - name: Test
        run: cargo test --workspace --locked
```

## 2. Replace `.github/workflows/ci.yml`

Drops to triggers plus a call. Note the `pull_request` trigger is **gone** (D10) — it existed
only for Dependabot, and Dependabot is going.

```yaml
name: CI

on:
  push:
    branches: [main]
    # A docs-only push has nothing here to check. Mixed commits still run.
    # This cannot let a tagged commit through unchecked: release.yml calls
    # checks.yml on every tag, whatever paths the commit touched.
    paths-ignore:
      - "docs/**"
      - "**/*.md"
  workflow_dispatch:

# One run per ref: pushing again while CI is still going cancels the older run
# rather than queueing a second 3-OS matrix nobody is waiting for.
concurrency:
  group: ${{ github.workflow }}-${{ github.ref }}
  cancel-in-progress: true

jobs:
  check:
    uses: ./.github/workflows/checks.yml
```

## 3. `.github/workflows/release.yml` changes

- [ ] **Add a `checks` job** calling the reusable workflow, and make `publish` wait for it.
      `build` keeps `needs: version` only, so builds and checks run concurrently:

  ```yaml
  jobs:
    version:
      ...

    # The release build does not run the tests (D7), so the checks run here
    # instead, at this exact commit. They gate `publish`, not `build`: the two
    # run side by side, and a failure means the artifacts exist but nothing is
    # published.
    #
    # Tags only. All this job protects is `publish`, and `publish` is itself
    # tag-only, so on a workflow_dispatch packaging run the matrix would be
    # three legs guarding nothing. Skipping it here also skips `publish`, which
    # is what the dispatch wanted anyway.
    checks:
      if: startsWith(github.ref, 'refs/tags/')
      uses: ./.github/workflows/checks.yml

    build:
      needs: version
      ...

    publish:
      needs: [version, checks, build]
      if: startsWith(github.ref, 'refs/tags/')
      ...
  ```

- [ ] **`version` job, `read` step (D13):** move the `x.y.z` regex inside the tag branch, so
      `workflow_dispatch` can build a tree mid-bump. The step body becomes:

  ```bash
  set -euo pipefail
  crate=$(grep -m1 '^version = ' Cargo.toml | cut -d'"' -f2)
  # workflow_dispatch has no tag, so there is nothing to compare against, and
  # it is allowed to build whatever version the tree has - packaging can then
  # be checked in the middle of a bump.
  if [[ "${GITHUB_REF}" == refs/tags/* ]]; then
    # Plain x.y.z only: the rpm tooling rejects a `-rc.1` suffix and cargo-deb
    # rewrites it to `~rc.1`, so the install scripts' asset names would no
    # longer line up.
    if ! [[ "$crate" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
      echo "version $crate is not a plain x.y.z release" >&2
      exit 1
    fi
    tag="${GITHUB_REF_NAME#v}"
    if [[ "$tag" != "$crate" ]]; then
      echo "tag $tag does not match Cargo.toml version $crate" >&2
      exit 1
    fi
  fi
  echo "version=$crate" >> "$GITHUB_OUTPUT"
  ```

- [ ] Nothing else. Action versions here are already current (`checkout@v7`,
      `upload-artifact@v7`, `download-artifact@v8`); `action-gh-release@v2` stays. The matrix,
      the Inno Setup step, `cargo-deb` / `cargo-generate-rpm`, `SHA256SUMS` and the
      release-notes body are untouched.

## 4. Delete `.github/dependabot.yml` (D10)

- [ ] `git rm .github/dependabot.yml`.
- [ ] Then, **in the GitHub UI or API, turn on Dependabot security alerts** for
      `toperux/t4-claude-session-browser`. This is the part of Dependabot worth having and it
      is currently off in at least one of the three repos (checked on git-ui, 2026-09-03);
      this repo was never checked. Settings → Advanced Security → Dependabot alerts, or:

  ```sh
  gh api -X PUT repos/toperux/t4-claude-session-browser/vulnerability-alerts
  gh api -X PUT repos/toperux/t4-claude-session-browser/automated-security-fixes
  ```

  Record below whether they were already on.

  **Done 2026-09-04. Both were OFF.** `GET repos/.../vulnerability-alerts` returned 404 and
  `automated-security-fixes` reported `{"enabled":false}`. Both `PUT`s were run; alerts now
  return 204 and `automated-security-fixes` reports `{"enabled":true,"paused":false}`. See
  the first item under *Deviations and findings* about what the second one implies.

## 5. Risks to verify

- **Reusable-workflow resolution.** `uses: ./.github/workflows/checks.yml` resolves at the
  caller's commit. On the first push this means `checks.yml` must exist in the same commit as
  the `ci.yml` that calls it — land section 1 and 2 in one commit, not two.
- **Job naming.** The matrix legs now appear as `check / ${{ matrix.os }}` (caller job name,
  then the reusable job's). Cosmetic; do not add `name:` overrides chasing the old labels.
- **A tagged release now costs a check matrix on top of the builds.** Wall-clock is unchanged
  (they run alongside `build`), but a release spends **three** more legs of Actions minutes
  than it did under round 1 — the check matrix is three legs on every repo, regardless of this
  repo's four-target build matrix. That is the price of the guarantee; it is not a mistake to
  be optimised away. `workflow_dispatch` runs are unaffected — `checks` is tag-gated.
- **Recovering from a failed check on a tag.** `build` will have succeeded and uploaded
  artifacts; `publish` will not have run, so no release exists and nothing is public. Fix the
  commit, delete the tag locally and on the remote (`git tag -d vX.Y.Z && git push origin
  :refs/tags/vX.Y.Z`), then re-tag. Do not re-run only the failed job to force a publish.
- **`ubuntu-22.04` retirement — still open, cross-repo.** Deprecated from 2026-09-17,
  brownouts 2027-03-23 / -03-30 / -04-06 / -04-13 (14:00–00:00 UTC), unsupported 2027-04-17
  (`actions/runner-images#14254`). Deliberate here for the glibc floor. **Not in scope for
  round 2** — do not switch it. The fix has to be picked once for all three.

## 6. Commit plan

1. `ci: run the checks from one reusable workflow` — sections 1 and 2 **in a single commit**
   (see Risks).
2. `release: gate publishing on the checks` — section 3.
3. `ci: drop dependabot` — section 4's file deletion. The alerts setting is a repo setting,
   not a commit.

Do not tag a release as part of this work.

## 7. Verification

Sections 1–4 are committed on `main` (locally, unpushed) as of 2026-09-04, in the order
section 6 gives, plus a fourth commit for this document. The alert settings are already
applied — that was an API call, not a commit. Everything that needs a runner is outstanding.

Checked locally instead of on a runner, where that was possible:

- [x] `cargo clippy --workspace --all-targets --locked -- -D warnings` and
      `cargo test --workspace --locked` both pass on Windows, so `Cargo.lock` is current and
      D11 will not turn the matrix red on the first push. 38 tests green.
- [x] The reworked `version` script (D13) was extracted and run against five cases: a
      dispatch on `0.2.7` and on a mid-bump `0.2.8-rc.1` both succeed and print the tree's
      version; a matching tag succeeds; a tag that disagrees with `Cargo.toml` and a
      non-`x.y.z` tag both still exit 1. So the regex now guards tags only, as intended.
- [x] All three workflow files parse, and `release.yml`'s job graph is `version` (no needs),
      `checks` (tag-gated, `uses: ./.github/workflows/checks.yml`), `build` (`needs:
      version`), `publish` (`needs: [version, checks, build]`, tag-gated).
- [x] No `pull_request` trigger anywhere under `.github/`, and `.github/dependabot.yml` is
      gone.

Still needing a runner:

- [ ] Push to a branch, then `workflow_dispatch` **CI** on it (the branch is not `main`, so
      the push trigger will not fire). Three legs green, shown as `check / <os>`.
- [ ] Confirm `Format` ran on `ubuntu-22.04` only.
- [ ] Confirm clippy and test both show `--locked` in the log.
- [ ] `workflow_dispatch` **Release** on the branch. `version` green and printing the tree's
      version; all four `build` legs green; `checks` **skipped** and `publish` skipped (both
      are tag-gated). This is also the D13 check: a dispatch on a non-`x.y.z` tree must now
      build rather than fail.
- [ ] The `build`-and-`checks`-run-concurrently behaviour cannot be observed from a dispatch
      run, since `checks` is skipped there. It is first exercised by a real tag; confirm the
      timings then.
- [ ] Confirm no `pull_request` trigger remains in either workflow and `.github/dependabot.yml`
      is gone.
- [ ] Merge to `main`; CI green on `main`.
- [x] Dependabot security alerts on for this repo. Were they already on? **No — both were
      off, and both are on now.** See section 4.

## 8. Deviations and findings

> Anything you changed that this plan did not ask for, and anything you noticed that the other
> two repos should probably also do. **Record here; do not act on it beyond this repo.** The
> next master pass reads this section. If this section is empty, say so explicitly.

Nothing in sections 1–4 was changed, added to, or skipped. The two workflows and the deleted
file match the plan exactly. Four findings, none acted on beyond this repo:

1. **D10's "no PRs" rationale does not survive the second command it prescribes.** The
   decision drops Dependabot because "its only output is PRs" and keeps security alerts as
   "a repo setting, no config file, no PRs, no Actions minutes". That is true of
   `vulnerability-alerts`, which only raises alerts. It is **not** true of
   `automated-security-fixes`: that is Dependabot security updates, and it opens PRs — the
   same `dependabot/**` branches D10 is removing the trigger for. Section 4 told me to enable
   both, so I did, but the result is that this repo can now receive PRs that no workflow
   checks, which is the exact hole round 2 exists to close. It is a much narrower hole than
   round 1's (only a real CVE opens one, and it is a merge nobody should be doing blind
   anyway), so it may well be the right trade. But the master pass should decide it on
   purpose: either enable `vulnerability-alerts` alone and get genuinely PR-free alerting, or
   keep both and drop the "no PRs" clause from D10's wording. Whichever way it goes, all
   three repos should match — the same two commands are in all three plans.
2. **Alerts were off here, as suspected.** Both endpoints were disabled before this pass, so
   that is two of three repos found off (git-ui on 2026-09-03, csb now). mdv is worth
   checking; if it is off too, the finding is that the setting has never been on anywhere,
   not that it drifted.
3. **Section 6's commit plan has no commit for this document.** Its three commits cover only
   the workflow files, yet section 8 is written to be read by a later pass, which requires the
   file to be in the repo. I added a fourth commit, `docs: record the round 2 plan and what
   implementing it found`, after the other three. Round 1 did the same thing (`6c3dd03`), so
   the plans should probably just list it.
4. **`checks.yml` inherits `contents: write` when release.yml calls it.** `release.yml` sets
   that permission at the workflow level for `publish`, and a called reusable workflow runs
   with the caller's token permissions, so the check matrix gets write access it has no use
   for. Harmless today — the steps only build and test — and out of scope besides. The tidy
   fix, if the master pass wants it, is `permissions: contents: read` on the job in
   `checks.yml`, which applies in both callers.

One version note, not a finding, since D14 settles it: `softprops/action-gh-release` is now at
v3 and all three repos deliberately stay on `@v2`, so that pin is knowingly one major behind.
`taiki-e/install-action@v2` and `Swatinem/rust-cache@v2` are both still current majors.
