# Security policy

## Reporting a vulnerability

Please **do not** open a public issue for security problems. Report them privately through
[GitHub security advisories](https://github.com/zweiund40GmbH/fundament-sdk/security/advisories/new)
("Report a vulnerability"). Include the affected version, a description and, if possible, steps to reproduce.

We acknowledge reports within five working days and keep you informed until a fix is released. Please give us
reasonable time to release a fix before disclosing the issue.

## Supported versions

Before `1.0`, only the latest released minor version receives security fixes.

## Scope

In scope: the crate `z40-fundament` (key verification, failure brake, feed parsing, storage, status endpoint)
and the protocol description. Configuration and operation of the apps that use the crate are the
responsibility of their operators.
