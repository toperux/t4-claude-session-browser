#!/bin/sh
# Downloads the latest csb tarball and installs the binary under ~/.local/bin.
# No root needed, and because that directory is writable by you, `csb update`
# can replace the binary in place later. Linux x86_64 and macOS.
#
#   curl -fsSL https://raw.githubusercontent.com/toperux/t4-claude-session-browser/main/installer/install-user.sh | sh
#
# CSB_VERSION=0.2.4 pins a release; CSB_INSTALL_DIR overrides ~/.local/bin.
set -eu

repo="toperux/t4-claude-session-browser"
dir="${CSB_INSTALL_DIR:-$HOME/.local/bin}"

os=$(uname -s); arch=$(uname -m)
case "$os-$arch" in
    Linux-x86_64)          target=x86_64-unknown-linux-gnu ;;
    Darwin-arm64)          target=aarch64-apple-darwin ;;
    Darwin-x86_64)         target=x86_64-apple-darwin ;;
    *)
        echo "install-user.sh: no prebuilt binary for $os $arch; see" >&2
        echo "  https://github.com/$repo/releases/latest" >&2
        exit 1 ;;
esac

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
[ -n "$version" ] || { echo "install-user.sh: could not determine the latest version" >&2; exit 1; }

file="csb-$target.tar.gz"
base="https://github.com/$repo/releases/download/v$version"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
echo "Downloading $base/$file"
curl -fsSL -o "$tmp/$file" "$base/$file"
curl -fsSL -o "$tmp/SHA256SUMS" "$base/SHA256SUMS"
if command -v sha256sum >/dev/null 2>&1; then sum="sha256sum"; else sum="shasum -a 256"; fi
(cd "$tmp" && grep " $file\$" SHA256SUMS | $sum -c --quiet -)
tar xzf "$tmp/$file" -C "$tmp"

mkdir -p "$dir"
# Move rather than copy over the top: a running csb (the GUI, say) keeps its
# old inode and is not clobbered mid-execution.
mv -f "$tmp/csb" "$dir/csb"
chmod 755 "$dir/csb"
# The binary is not signed; without this macOS refuses to run a downloaded one.
[ "$os" = Darwin ] && xattr -d com.apple.quarantine "$dir/csb" 2>/dev/null || true

# A menu entry for the desktop app on Linux. The tarball's csb.desktop has
# Exec=csb gui; make it absolute so it works even when $dir is not on the
# session's PATH. Older releases ship no desktop entry - then there is none.
if [ "$os" = Linux ] && [ -f "$tmp/csb.desktop" ]; then
    share="${XDG_DATA_HOME:-$HOME/.local/share}"
    mkdir -p "$share/applications" "$share/icons/hicolor/256x256/apps"
    sed "s|^Exec=csb |Exec=\"$dir/csb\" |" "$tmp/csb.desktop" > "$share/applications/csb.desktop"
    cp "$tmp/csb.png" "$share/icons/hicolor/256x256/apps/csb.png"
fi

echo "Installed csb $version to $dir/csb"
case ":$PATH:" in
    *":$dir:"*) ;;
    *)
        echo
        echo "$dir is not on your PATH. Add it, e.g. in ~/.bashrc or ~/.zshrc:"
        echo "  export PATH=\"$dir:\$PATH\""
        ;;
esac
echo "Later: 'csb update' upgrades in place."
