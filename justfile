# All commands of this repository; CI runs the same recipes (local = CI).
set shell := ["bash", "-euo", "pipefail", "-c"]

# Published crates, in dependency order (z40-fundament-http depends on z40-fundament).
crates := "z40-fundament z40-fundament-http"
msrv := "1.94"

# Full quality gate: format, lint (every feature alone and all together), tests, docs, file sizes, MSRV.
check: fmt versions clippy test doc lines msrv

# Formatting check.
fmt:
    cargo fmt --all -- --check

# Features of a crate (from its manifest, without `default`).
features crate:
    @awk '/^\[features\]/{f=1;next} /^\[/{f=0} f && /^[a-z]/{print $1}' crates/{{crate}}/Cargo.toml | grep -vx default || true

# The workspace's own requirement on z40-fundament (`[workspace.dependencies]`) must be `=<version>`.
versions:
    #!/usr/bin/env bash
    set -euo pipefail
    version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)"
    required="$(sed -n 's/^z40-fundament = { version = "\([^"]*\)".*/\1/p' Cargo.toml)"
    if [ "$required" != "=$version" ]; then
        echo "error: [workspace.dependencies] z40-fundament is \"$required\", expected \"=$version\""
        exit 1
    fi
    echo "versions: z40-fundament requirement =$version"

# Clippy with warnings as errors, per crate: no features, each feature alone; then all features.
clippy:
    #!/usr/bin/env bash
    set -euo pipefail
    for c in {{crates}}; do
        if cargo hack --version >/dev/null 2>&1; then
            cargo hack clippy -p "$c" --each-feature --all-targets --locked -- -D warnings
        else
            echo "== clippy -p $c --no-default-features"
            cargo clippy -p "$c" --all-targets --no-default-features --locked -- -D warnings
            for f in $(just features "$c"); do
                echo "== clippy -p $c --features $f"
                cargo clippy -p "$c" --all-targets --no-default-features --features "$f" --locked -- -D warnings
            done
        fi
    done
    cargo clippy --workspace --all-targets --all-features --locked -- -D warnings

# Tests with all features. Integration tests need TEST_DATABASE_URL (MariaDB server URL without
# database name, e.g. mysql://root:pw@127.0.0.1:3306); without it they print `[skip]`.
test:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -z "${TEST_DATABASE_URL:-}" ]; then
        echo "[skip] MariaDB integration tests (replica_mysql, run_once, feed_source_mysql, feed_source_assign, feed_round_trip): TEST_DATABASE_URL not set"
    fi
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
        cargo +{{msrv}} check --workspace --all-features --locked
    else
        echo "[skip] msrv: toolchain {{msrv}} not installed (rustup toolchain install {{msrv}})"
    fi

# Dependency audit: advisories, licenses, sources (needs cargo-deny).
deny:
    cargo deny check advisories bans licenses sources

# Regenerate the generated tables in docs/configuration.md (one section per crate).
config-doc:
    UPDATE_CONFIG_DOC=1 cargo test --workspace --all-features --locked --test config_doc

# Packaging sanity check of all crates together (nothing is published). Packaging them in one call lets
# cargo resolve z40-fundament-http against the z40-fundament of this workspace, even before it is published.
package:
    cargo package $(printf -- '-p %s ' {{crates}}) --locked --no-verify
