#!/bin/sh
# Downloads the latest csb .deb or .rpm and installs it with the system
# package manager. Linux x86_64 only.
#
#   curl -fsSL https://raw.githubusercontent.com/toperux/t4-claude-session-browser/main/installer/install.sh | sh
#
# Set CSB_VERSION=0.2.4 to pin a release instead of taking the latest.
set -eu

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
elif command -v zypper >/dev/null 2>&1; then
    kind=rpm; pm="zypper"
elif command -v yum >/dev/null 2>&1; then
    kind=rpm; pm="yum"
else
    echo "install.sh: no apt-get, dnf, zypper or yum found; use the tarball instead" >&2
    exit 1
fi

version="${CSB_VERSION:-}"
version="${version#v}"
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
# mktemp gives 0700; apt downloads as the `_apt` user and warns when it
# cannot read the package.
chmod 755 "$tmp"
trap 'rm -rf "$tmp"' EXIT

# The package's exact name comes from the release's own checksum list rather
# than being guessed here, and the list then verifies the download.
curl -fsSL -o "$tmp/SHA256SUMS" "$base/SHA256SUMS"
file=$(awk '{ print $2 }' "$tmp/SHA256SUMS" | grep -m1 "\\.$kind\$" || true)
[ -n "$file" ] || { echo "install.sh: release v$version has no .$kind package" >&2; exit 1; }

echo "Downloading $base/$file"
curl -fsSL -o "$tmp/$file" "$base/$file"
(cd "$tmp" && grep " $file\$" SHA256SUMS | sha256sum -c --quiet -)

sudo=""
[ "$(id -u)" -eq 0 ] || sudo="sudo"

echo "Installing $file"
case "$kind" in
    deb) $sudo apt-get install -y "$tmp/$file" ;;
    rpm) $sudo "$pm" install -y "$tmp/$file" ;;
esac

echo "Installed csb $version. Run 'csb' for the desktop app or 'csb tui' in a terminal."
echo "To upgrade later, run this script again."
