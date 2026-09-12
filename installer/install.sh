#!/bin/sh
# Downloads the latest csb .deb or .rpm and installs it with the system
# package manager. Linux x86_64 only.
#
#   curl -fsSL https://raw.githubusercontent.com/toperux/t4-claude-session-browser/main/installer/install.sh | sh
#
# Set CSB_VERSION=0.2.4 to pin a release instead of taking the latest.
set -eu

# A pipe to `sh` executes each command as it parses, so a truncated download
# must not run a prefix of this script. Nothing happens until the last line.
main() {
    repo="toperux/t4-claude-session-browser"

    if [ "$(uname -s)" != "Linux" ] || [ "$(uname -m)" != "x86_64" ]; then
        echo "install.sh: only Linux x86_64 packages are published; see the tarballs at" >&2
        echo "  https://github.com/$repo/releases/latest" >&2
        exit 1
    fi

    if command -v apt-get >/dev/null 2>&1; then
        kind=deb
    elif command -v dnf >/dev/null 2>&1; then
        kind=rpm; pm="dnf"
    elif command -v yum >/dev/null 2>&1; then
        kind=rpm; pm="yum"
    else
        echo "install.sh: no apt-get, dnf or yum found; use installer/install-user.sh" >&2
        echo "  (the tarball) instead" >&2
        exit 1
    fi

    # Same block as installer/install-user.sh; keep them in step.
    version="${CSB_VERSION:-}"
    version="${version#v}"
    pinned=""
    [ -z "$version" ] || pinned=1
    if [ -z "$version" ]; then
        # The redirect target of /releases/latest is /releases/tag/v<version>;
        # reading it avoids the API and its rate limit. Anything else (no
        # releases, all of them pre-releases) lands on a page that is not a tag.
        latest=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$repo/releases/latest")
        case "$latest" in
            */releases/tag/v*) version="${latest##*/releases/tag/v}" ;;
        esac
    fi
    [ -n "$version" ] || { echo "install.sh: could not determine the latest version" >&2; exit 1; }

    base="https://github.com/$repo/releases/download/v$version"
    tmp=$(mktemp -d)
    # mktemp gives 0700 and a strict umask leaves the download 0600; apt reads
    # as the `_apt` user and warns when it cannot get at the package, so the
    # directory and the file both have to be readable.
    chmod 755 "$tmp"
    trap 'rm -rf "$tmp"' EXIT

    # The package's exact name comes from the release's own checksum list rather
    # than being guessed here, and the list then verifies the download.
    curl -fsSL -o "$tmp/SHA256SUMS" "$base/SHA256SUMS"
    file=$(awk '{ print $2 }' "$tmp/SHA256SUMS" | grep -m1 "\\.$kind\$" || true)
    # The name becomes a local path below, so it must be a plain file name; a
    # leading dot would let `..` out of $tmp before the checksum is checked.
    case "$file" in
        "") echo "install.sh: release v$version has no .$kind package" >&2; exit 1 ;;
        */*|.*) echo "install.sh: bad asset name in SHA256SUMS" >&2; exit 1 ;;
    esac

    echo "Downloading $base/$file"
    curl -fsSL -o "$tmp/$file" "$base/$file"
    chmod 644 "$tmp/$file"
    (cd "$tmp" && grep " $file\$" SHA256SUMS | sha256sum -c --quiet -)

    sudo=""
    [ "$(id -u)" -eq 0 ] || sudo="sudo"

    echo "Installing $file"
    # A pinned CSB_VERSION can be older than what is installed. dnf4 has no
    # --allow-downgrade and fails loudly on it, which beats a silent no-op.
    case "$kind" in
        deb) $sudo apt-get install -y ${pinned:+--allow-downgrades} "$tmp/$file" ;;
        rpm) $sudo "$pm" install -y ${pinned:+--allow-downgrade} "$tmp/$file" ;;
    esac

    echo "Installed csb $version. Run 'csb' for the desktop app or 'csb tui' in a terminal."
    echo "To upgrade later, run this script again."
}

main "$@"
