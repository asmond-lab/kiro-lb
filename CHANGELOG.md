# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.7](https://github.com/minpeter/kiro-lb/compare/v0.2.6...v0.2.7) - 2026-09-30

### Fixed

- give every account its own stable machine id instead of one id shared by all accounts
- present the current Kiro IDE client (1.1.70) in every user agent, without the CLI marker
- send management calls (model list, usage limits) with the same shape and headers as the Kiro IDE
- send Claude reasoning effort without the extra thinking block, as the Kiro IDE does
- keep `agentTaskType: vibe` in spec mode, as the Kiro IDE does
- keep `additionalProperties` in tool schemas

## [0.2.6](https://github.com/minpeter/kiro-lb/compare/v0.2.5...v0.2.6) - 2026-09-30

### Fixed

- stop counting thinking signatures as prompt tokens in the payload guard (#93)
- keep mid-conversation system messages in place so the prompt prefix stays stable and Kiro's prompt cache is reused
- keep idle upstream connections open for 30 minutes with HTTP/2 keepalive pings

## [0.2.5](https://github.com/minpeter/kiro-lb/compare/v0.2.4...v0.2.5) - 2026-09-29

### Fixed

- flag social accounts a new social login signed out
- match Kiro CLI additive credit metering

### Other

- drop stray blank line from static/index.html
- keep LF line endings in static/index.html

## [0.2.4](https://github.com/minpeter/kiro-lb/compare/v0.2.3...v0.2.4) - 2026-09-28

### Fixed

- keep each device-login account's credential separate: login identity no longer derives from the shared profile ARN (#86)
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
