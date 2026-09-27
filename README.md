<p align="center">
  <img src="assets/kiro-lb-banner.png" alt="Kiro LB" width="100%">
</p>

# kiro-lb

OpenAI- and Anthropic-compatible gateway for Kiro (Amazon Q Developer /
CodeWhisperer), with multi-account load balancing and an operations dashboard.

## License

**AGPL-3.0** — see [`LICENSE`](LICENSE) and [`NOTICE.md`](NOTICE.md).

Based on [jwadow/kiro-gateway](https://github.com/jwadow/kiro-gateway)
(Copyright (C) 2025 Jwadow). This tree adds multi-account routing, dashboard,
and related changes (Copyright (C) 2026 minpeter).

## Disclaimer

This project is **not** affiliated with, endorsed by, or sponsored by Amazon
Web Services, Inc. or Anthropic. Use of upstream Kiro / Amazon Q services is
subject to **your** AWS / product terms of service. You are solely responsible
for how you obtain credentials and for compliance with applicable terms and law.

## Quick start

### Standalone binary

Download from the [releases page](https://github.com/minpeter/kiro-lb/releases):

| Platform | File |
|---|---|
| Windows x64 | `kirolb-windows-x64.exe` |
| Windows ARM64 | `kirolb-windows-arm64.exe` |
| Linux x64 (static, any distro) | `kirolb-linux-x64.tar.gz` |
| Linux ARM64 (static, any distro) | `kirolb-linux-arm64.tar.gz` |

The Windows files are the executable itself: download and run. The Linux
files are a tarball so the executable bit survives the download:

```bash
tar -xzf kirolb-linux-x64.tar.gz
./kirolb-linux-x64
```

On first run it creates `.env` and `.env.example` with generated credentials
and prints them. Open http://localhost:8000 and add accounts with device login.
`SHA256SUMS` in the release lists every file's checksum.

### Docker

A multi-arch image (`linux/amd64`, `linux/arm64`) is published to GHCR:

```bash
docker run -d --name kiro-lb -p 8000:8000   -v "$PWD/data:/app/data" --env-file .env   ghcr.io/minpeter/kiro-lb:main
```

Or with the bundled compose file, which reads `.env` and keeps `data/` on the host:

```bash
docker compose up -d
```

### From source

```bash
cd frontend && bun install && bun run build && cd ..
cargo build --release          # target/release/kirolb(.exe)
```

See `.env.example` and `AGENTS.md` for configuration details.
Issues: https://github.com/minpeter/kiro-lb/issues

## Release maintenance

Release-plz opens or updates a release PR after changes reach `main`. Review its
version and changelog, then merge it to approve a release. Use Conventional
Commits (`fix:`, `feat:`, and `!` / `BREAKING CHANGE:`) for useful release notes.
The baseline is `v0.2.0`; versions and tags are not reset. This is GitHub-only
distribution: `git_only = true` disables publishing to crates.io.

Before merging the automation setup, add the repository Actions secret
`RELEASE_PLZ_TOKEN`: a fine-grained personal access token scoped only to this
repository, with **Contents: read/write** and **Pull requests: read/write**.
Use a bot account if available, set an expiry, and rotate it before expiry.
Do not put the token in a file or PR. This separate token lets release PRs
trigger normal PR CI; the default `GITHUB_TOKEN` would suppress those events.
The release PR job fails explicitly if the secret is missing.

The release workflow uses the default `GITHUB_TOKEN` to create a tag and a
**draft** release, then directly calls the existing build workflow for that
exact tag. It does not depend on a bot-created tag triggering another workflow.
Rust/frontend checks and all four binary builds must pass before upload. The
publisher downloads the uploaded assets, verifies `SHA256SUMS`, and only then
makes the release public. Published releases are never overwritten.

If a release build or upload fails, the draft remains unpublished. Re-run failed
jobs, or run **Release automation** manually from `main` with the existing draft
tag (for example `v0.2.1`). Recovery rejects published releases and tags outside
`main` history. It rebuilds the tagged source using the selected workflow's
packaging logic. Do not move or reuse published version tags.

The existing `main` Docker build remains independent; automatic binary releases
do not add a versioned Docker image or deploy/restart any running server.
