fmt_toolchain := "nightly"

default:
    @just --list

fmt:
    RUSTFMT="$(rustup which --toolchain {{fmt_toolchain}} rustfmt)" rustup run {{fmt_toolchain}} cargo fmt --manifest-path niri/Cargo.toml --all
    cargo fmt --manifest-path nirius/Cargo.toml --all

# Run Clippy; warnings fail the check.
lint:
    cargo clippy --locked --manifest-path niri/Cargo.toml --workspace --exclude niri-visual-tests --all-targets -- -D warnings
    cargo clippy --locked --manifest-path nirius/Cargo.toml --all-targets -- -D warnings

# Run tests with an isolated runtime directory; EGL tests need a graphics environment.
test:
    #!/usr/bin/env bash
    set -euo pipefail
    XDG_RUNTIME_DIR="$(mktemp -d)"
    export XDG_RUNTIME_DIR
    trap 'rm -rf "$XDG_RUNTIME_DIR"' EXIT
    cargo test --locked --manifest-path niri/Cargo.toml --workspace --exclude niri-visual-tests -- --skip=::egl
    cargo test --locked --manifest-path nirius/Cargo.toml

# Build release binaries without installing or changing the current desktop.
build:
    cargo build --locked --release --manifest-path niri/Cargo.toml
    cargo build --locked --release --manifest-path nirius/Cargo.toml
