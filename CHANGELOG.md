# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.4](https://github.com/minpeter/kiro-lb/compare/v0.2.3...v0.2.4) - 2026-09-28

### Fixed

- group banned accounts below paused accounts

### Other

- extract account grouping for direct tests

## [0.2.3](https://github.com/minpeter/kiro-lb/compare/v0.2.2...v0.2.3) - 2026-09-28

### Added

- check releases and install standalone updates from dashboard

### Fixed

- use official model provider marks
- seed dashboard reload detection from served document version
- preserve stop intent and reload dashboards on version changes
- guard failed Windows replacements and recover update targets

### Other

- Move appearance controls into settings
- Merge main and preserve dashboard update review fixes

## [0.2.2](https://github.com/minpeter/kiro-lb/compare/v0.2.1...v0.2.2) - 2026-09-28

### Added

- Support native GPT reasoning effort controls without synthesizing reasoning text.
- Route requests by model capability and available account capacity while retaining suitable session affinity.
- Add reversible Codex CLI and Claude Code setup, diagnosis, status, and restoration commands. Automatic setup and restoration are Linux-only; read-only commands remain cross-platform.

### Fixed

- Keep catalog refresh and failed-account recovery off the request path, with single-flight initialization and bounded warm-up waits.
- Validate authentication and API regions before outbound requests, reject malformed credential imports, and preserve the last-good credential snapshot on source-read failures.
- Meter credits per physical generation on the originating account, handle repeated snapshots without double counting, and distinguish valid non-credit completion markers from malformed metering frames.
- Bind durable authentication and quota state to the current login, discard stale refresh results, and apply the same quota freshness rules to account eligibility and routing weight.
- Preserve concurrent client edits and recovery journals during setup failures, bound diagnostic responses and configuration files, clear competing Claude credentials and provider selectors, and reject unsafe plaintext proxy configurations.
- Avoid exposing account identifiers in capacity errors and prevent late requests from mutating replacement accounts or refreshing their affinity.

### Other

- Use the short-lived GitHub Actions token for draft releases, validating all four binary artifacts and their checksums before publication.

## [0.2.1](https://github.com/minpeter/kiro-lb/compare/v0.2.0...v0.2.1) - 2026-09-27

### Fixed

- *(debug)* sanitize opaque inputs and exported stream content
- *(debug)* address review on sanitizer, failure classification, export
- make model discovery part of activation and readiness
- *(debug)* redact all request text when content capture is off
