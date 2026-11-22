#!/usr/bin/env bash
set -euo pipefail

case "$1" in
  misari|wallpaperd) recipe="$1-git" ;;
  galscaler) recipe=$1 ;;
  *) printf 'Unknown package: %s\n' "$1" >&2; exit 2 ;;
esac

pacman -Syu --needed --noconfirm git
useradd --create-home --uid "${BUILD_UID:-1000}" builder
printf 'builder ALL=(root) NOPASSWD: /usr/bin/pacman\n' > /etc/sudoers.d/builder
chmod 0440 /etc/sudoers.d/builder
install -d -o builder /build /packages

commit=$(runuser -u builder -- git -C /checkout rev-parse HEAD)
runuser -u builder -- git clone --no-hardlinks --no-checkout /checkout /build
runuser -u builder -- git -C /build checkout --detach "$commit"

cd "/build/packaging/$recipe"
export MISARI_CHECKOUT=/build PKGDEST=/packages
runuser -u builder -- makepkg --syncdeps --noconfirm --check
