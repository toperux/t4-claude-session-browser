# csb — Claude Session Browser

[![CI](https://github.com/toperux/t4-claude-session-browser/actions/workflows/ci.yml/badge.svg)](https://github.com/toperux/t4-claude-session-browser/actions/workflows/ci.yml)

Browse, preview and delete Claude Code sessions. One Rust binary with three faces:
a desktop GUI, a terminal UI, and scriptable subcommands.

## Install

**Windows** — run `csb-setup-<version>-x64.exe` from the [latest release][releases].
It installs for the current user only, so there is no admin prompt; it adds `csb`
to your `PATH` and puts the desktop app in the Start Menu. Open a **new** terminal
afterwards, or `PATH` won't have caught up.

**Linux** — one command fetches the latest `.deb` or `.rpm` and installs it with
`apt`/`dnf`/`zypper` (it will ask for `sudo`):

```sh
curl -fsSL https://raw.githubusercontent.com/toperux/t4-claude-session-browser/main/installer/install.sh | sh
```

Or download `csb_<version>-1_amd64.deb` / `csb-<version>-1.x86_64.rpm` from the
release yourself and `sudo apt install ./csb_*.deb` or `sudo dnf install ./csb-*.rpm`.
Either way `csb` lands in `/usr/bin` and the desktop app in your application
menu. Packaged installs are upgraded by re-running the command (or installing the
next package); `csb update` refuses to touch a file the package manager owns.
The rpm names Fedora's library packages, so it may not install on openSUSE.

**Linux or macOS, no root** — installs the tarball's binary to `~/.local/bin`
(override with `CSB_INSTALL_DIR`). Since that is yours to write, `csb update`
upgrades it in place afterwards:

```sh
curl -fsSL https://raw.githubusercontent.com/toperux/t4-claude-session-browser/main/installer/install-user.sh | sh
```

**Anything else** — grab the archive for your platform from the same place, unpack
it, and put `csb` on your `PATH`. `SHA256SUMS` alongside the assets lists their hashes.

[releases]: https://github.com/toperux/t4-claude-session-browser/releases/latest

Or build it:

```sh
cargo build --release      # target/release/csb
```

On Windows the release also carries `csb-gui.exe`. It is the same app as `csb`
with no arguments, built as a GUI binary so a shortcut to it opens the window
without a console sitting behind it; `csb.exe` stays the one to use in a terminal.

## Use

```sh
csb                        # desktop GUI (default)
csb tui                    # terminal UI
csb projects               # list projects
csb list [-p <project>] [--sort date|size|msgs] [--json]
csb show <id> [--raw] [--sidechains]
csb delete <id>... [--older-than 30d] [-p <project>] [--dry-run] [-y] [--force]
csb update [--check] [--force]   # install the latest release
```

`<id>` accepts any unique prefix of a session UUID. `--claude-dir <DIR>` points the
tool at a different config tree (also honours `CLAUDE_CONFIG_DIR`); everything
defaults to `~/.claude`.

### TUI keys

`tab` cycle panes · `j/k` move · `g/G` top/bottom · `space` mark · `a`/`A` mark all/none
`d` delete · `/` filter · `s` cycle sort · `r` reload · `q`/`ctrl-c` quit

### GUI

Click a session to preview it, tick checkboxes to mark several (ctrl-click toggles,
shift-click selects a range). Delete acts on the marked sessions, or on the previewed
one when nothing is marked.

## What "delete" removes

A session is more than its transcript, so deleting one takes all of it:

- `projects/<slug>/<id>.jsonl` — the transcript
- `projects/<slug>/<id>/` — subagent sidechains
- `session-env/<id>/`
- `file-history/<id>/`

Everything goes to the **OS recycle bin**, so a mistake is recoverable through the
usual restore path. `history.jsonl` is shared across sessions and is never touched.

Two guards: nothing outside the resolved claude dir can be deleted, and a session
touched in the last five minutes is flagged as possibly live — the UIs warn, and the
CLI refuses without `--force`.

## Updating

`csb update` downloads the release built for your platform and replaces the
executables in place, so `csb` needs write access to its own directory — a copy
in `C:\Program Files` or `/usr/local/bin` will need elevation, whereas the
Windows installer's `%LOCALAPPDATA%` location just works. On Windows both
`csb.exe` and `csb-gui.exe` are replaced, so close the desktop app first —
Windows won't overwrite a running executable, and the update will say so if it
hits one. Nothing restarts on its own; the new version applies next launch.

If an update does get interrupted that way, `csb update` will then report
itself up to date while the other binary is still behind, because each check
compares against the running binary's own version. `csb update --force`
reinstalls the latest release regardless and is the way out of that.

The GUI additionally checks once a day on startup and, if there is something
newer, shows a dismissible banner with an **Update** button. **Check for updates**
in the settings window (⚙ at the right of the toolbar, which also shows the
installed version) runs the check immediately, ignoring the daily throttle. A
failed check is silent — offline looks the same as up to date. Set `CSB_NO_UPDATE_CHECK=1` to
disable checking entirely, including `csb update --check`; an explicit `csb update`
still installs.

One cosmetic wart if you used the installer: a self-update swaps the binaries but
doesn't touch the uninstall registry entry, so Add/Remove Programs keeps showing
the version you installed. Running the new installer resyncs it.

## WSL

The Linux build runs under WSLg, with two adjustments it makes on its own:

- If resizing or snapping the window makes it vanish ("Broken pipe" in the
  terminal), that is WSLg's Wayland compositor rejecting a frame. `CSB_X11=1 csb`
  goes through XWayland instead, which does not have the problem but draws a
  plainer window frame. It needs `libxkbcommon-x11-0` (`sudo apt install
  libxkbcommon-x11-0`; the .deb/.rpm pull it in).
- WSLg reports 100% scaling whatever Windows is set to, so `csb` reads the
  Windows display scale from the registry (via `reg.exe`) and zooms to match.
  `CSB_SCALE=1.25` (or 1.5, 2) overrides that, e.g. for a non-primary monitor.

Sessions on a Windows drive (`--claude-dir /mnt/c/Users/you/.claude`) can be
browsed but not deleted from WSL: the Recycle Bin is out of reach there, so
`csb` refuses rather than stranding files in a `.Trash-1000` on `C:`. Delete
those from the Windows build.

## Notes

Sessions have no stored title, so one is derived: a `custom-title` record if present,
then `agent-name`, then the first real user prompt (CLI wrappers, `/clear`-style bare
commands and continuation preambles are seen through). Sessions with no messages at
all show as `(empty session)` — usually the first thing worth pruning.

Indexing reads all transcripts once (~300 MB in well under a second) and caches the
result by size+mtime, so later runs are instant.

## License

MIT — see [LICENSE](LICENSE).
