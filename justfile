# All commands of this repository; CI runs the same recipes (local = CI).
set shell := ["bash", "-euo", "pipefail", "-c"]

crate := "z40-fundament"
manifest := "crates/z40-fundament/Cargo.toml"
msrv := "1.94"

# Full quality gate: format, lint (every feature alone and all together), tests, docs, file sizes, MSRV.
check: fmt clippy test doc lines msrv

# Formatting check.
fmt:
    cargo fmt --all -- --check

# Features of the crate (from its manifest, without `default`).
features:
    @awk '/^\[features\]/{f=1;next} /^\[/{f=0} f && /^[a-z]/{print $1}' {{manifest}} | grep -vx default || true

# Clippy with warnings as errors: no features, each feature alone, all features.
clippy:
    #!/usr/bin/env bash
    set -euo pipefail
    if cargo hack --version >/dev/null 2>&1; then
        cargo hack clippy -p {{crate}} --each-feature --all-targets --locked -- -D warnings
    else
        cargo clippy -p {{crate}} --all-targets --no-default-features --locked -- -D warnings
        for f in $(just features); do
            echo "== clippy --features $f"
            cargo clippy -p {{crate}} --all-targets --no-default-features --features "$f" --locked -- -D warnings
        done
    fi
    cargo clippy --workspace --all-targets --all-features --locked -- -D warnings

# Tests with all features. Integration tests need TEST_DATABASE_URL (MariaDB server URL without
# database name, e.g. mysql://root:pw@127.0.0.1:3306); without it they print `[skip]`.
test:
    cargo test --workspace --all-features --locked

# API docs with warnings as errors.
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --locked

# File sizes: warning above 300 lines, error above 400.
lines:
    scripts/check-lines.sh

# Minimum supported Rust version. Skipped visibly when the toolchain is not installed (CI has it).
msrv:
    #!/usr/bin/env bash
    set -euo pipefail
    if rustup run {{msrv}} rustc --version >/dev/null 2>&1; then
        cargo +{{msrv}} check -p {{crate}} --all-features --locked
    else
        echo "[skip] msrv: toolchain {{msrv}} not installed (rustup toolchain install {{msrv}})"
    fi

# Dependency audit: advisories, licenses, sources (needs cargo-deny).
deny:
    cargo deny check advisories bans licenses sources

# Regenerate the generated table in docs/configuration.md.
config-doc:
    UPDATE_CONFIG_DOC=1 cargo test -p {{crate}} --all-features --locked --test config_doc

# Packaging sanity check (nothing is published).
package:
    cargo package -p {{crate}} --locked --no-verify
