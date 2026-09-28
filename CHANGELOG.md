# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.2](https://github.com/minpeter/kiro-lb/compare/v0.2.1...v0.2.2) - 2026-09-28

### Added

- route requests by model capability

### Fixed

- join scheduled account initialization
- redact account keys from capacity errors
- recover uninitialized accounts in background
- route around saturated accounts
- keep catalog refresh off request path
- refresh unknown model catalogs
- isolate late account failures
- commit affinity only for live accounts

### Other

- reconcile login state with reviewed gateway improvements
- Merge pull request #76 from minpeter/improve/credit-metering
- Merge pull request #74 from minpeter/improve/region-validation
- replace release PAT with GITHUB_TOKEN

## [0.2.1](https://github.com/minpeter/kiro-lb/compare/v0.2.0...v0.2.1) - 2026-09-27

### Fixed

- *(debug)* sanitize opaque inputs and exported stream content
- *(debug)* address review on sanitizer, failure classification, export
- make model discovery part of activation and readiness
- *(debug)* redact all request text when content capture is off
