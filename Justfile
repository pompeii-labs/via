#!/usr/bin/env just --justfile
# https://github.com/casey/just

export RUST_BACKTRACE := "1"

default:
    just --list

clean:
    cargo clean

build:
    cargo build

release:
    cargo build --locked --release

dev:
    cargo fmt
    cargo clippy --all-targets -- -D warnings

check:
    cargo fmt -- --check
    cargo clippy --all-targets -- -D warnings
    cargo test --locked --all-targets
    cargo build --locked --release

test:
    cargo test --locked --all-targets

run *ARGS:
    cargo run -- {{ARGS}}

fix:
    cargo fix --allow-staged --all-targets
    cargo clippy --fix --allow-staged --all-targets
    cargo fmt
