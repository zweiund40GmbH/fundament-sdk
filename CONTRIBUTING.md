# Contributing

Thank you for helping. This file describes how code gets into this repository.

## Principles

- **Clear before clever.** Small files (warning above 300 lines, error above 400), small functions, telling
  names. No trick without a comment, no abstraction that is used only once.
- **English** for code, comments, docs and API names. Human-readable texts that are part of the wire format
  (status texts, error messages of the status endpoint) follow `docs/protocol.md`.
- **Every public item is documented** (`missing_docs = "deny"`): *what* and *why*, not *how*.
- **No `unwrap()`/`expect()` outside tests**, `unsafe` is forbidden. Errors are `thiserror` types; they never
  carry secrets, response bodies or record contents.
- **Tests for every new behaviour**: unit tests next to the code, integration tests against real services
  (`TEST_DATABASE_URL`). Without the service a test prints `[skip]` – never skip silently. Each test creates
  its own data (its own database) and removes it.
- **Protocol first:** a change of the wire format starts in `docs/protocol.md`, then the code.
- **One source of truth:** `docs/configuration.md` is generated from the code (`just config-doc`) and checked
  by a test.
- **New dependencies** only with a reason in the commit message; prefer maintained, widely used crates. TLS is
  rustls only, SQL uses bind parameters only.

## Security

- No secrets, credentials, customer data or internal host names in code, tests, issues or commits. Use
  `example.com` and neutral names (`crm`, `billing`) in examples.
- Secrets are `SecretString` (redacted `Debug`), configuration is read through `from_lookup` functions.
- Report vulnerabilities privately, see [`SECURITY.md`](SECURITY.md).

## Development

```bash
just check        # the full gate: fmt, clippy (each crate, each feature alone and all), tests, docs, file sizes, MSRV
just test         # tests only
just config-doc   # regenerate docs/configuration.md
just deny         # cargo-deny (advisories, licenses, sources)
```

Integration tests need a MariaDB server; the tests create and drop their own databases:

```bash
docker run -d --name fundament-sdk-db --tmpfs /var/lib/mysql -e MARIADB_ROOT_PASSWORD=test -p 33077:3306 mariadb:10.11
TEST_DATABASE_URL=mysql://root:test@127.0.0.1:33077 just test
```

Without `TEST_DATABASE_URL`, `just test` prints `[skip]` and the database tests return early; CI fails if
the variable is empty.

The toolchain for development is pinned in `rust-toolchain.toml`; the minimum supported Rust version is
`rust-version` in `Cargo.toml` and is checked in CI.

## Commits and pull requests

- One commit per logical change, imperative subject line (`Add replica table prefix`), body with the reason.
- Reference issues with `Fixes #12` or `Refs #12`.
- `main` is the default branch; changes arrive through pull requests with a green CI.
- Update `CHANGELOG.md` (section *Unreleased*) for every user-visible change.

## Release process

1. Move the *Unreleased* entries in `CHANGELOG.md` to a new version section and bump `version` in the
   workspace `Cargo.toml` (SemVer; before `1.0` a breaking change bumps the minor version). Both crates share
   the version: set the `z40-fundament` requirement in `[workspace.dependencies]` of the workspace
   `Cargo.toml` to the same version, exactly (`=X.Y.Z`); `just versions` (part of `just check`) fails
   otherwise.
2. Merge to `main`, then tag the commit `vX.Y.Z` and push the tag.
3. The `release` workflow checks that the tag is on `main` and matches the version, runs the gate,
   publishes `z40-fundament` and then `z40-fundament-http` to crates.io (a crate whose version is already
   published is skipped, so a failed run can be repeated) and creates the GitHub release from the changelog
   section.

Publishing uses crates.io **Trusted Publishing** (OIDC, no stored token): the repository, the workflow
`release.yml` and the environment `release` are registered as trusted publisher of the crate on crates.io.
Each crate is registered on its own. The first version of a new crate (e.g. `z40-fundament-http`) cannot be
published that way; for it (or if OIDC is unavailable) set the repository secret `CARGO_REGISTRY_TOKEN` (a
token scoped to publishing the crates of this repository), which then takes precedence. Remove the secret
again afterwards and register the trusted publisher of the new crate.
