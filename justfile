# Everything CI checks, in one go.
default: fmt-check clippy test doc machete deny

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

clippy:
    cargo clippy --workspace --all-targets --locked -- -D warnings

build:
    cargo build --workspace

release:
    cargo build --workspace --release

test:
    cargo test --workspace --locked

doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

# Dependencies a crate declares but never names.
machete:
    cargo machete

# Known advisories in Cargo.lock (deny.toml).
deny:
    cargo deny --locked check advisories

# Dry-run of what release-plz publishes.
package:
    cargo package --workspace --locked

# Coverage report (needs `cargo install cargo-llvm-cov`). Informational, not gated.
coverage:
    cargo llvm-cov --workspace --lcov --output-path lcov.info --ignore-filename-regex "src/main\.rs"
    cargo llvm-cov report --ignore-filename-regex "src/main\.rs"
