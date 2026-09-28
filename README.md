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

At startup and every hour, the gateway checks GitHub's latest stable
release in the background. A newer release produces a log notice with a download
link. Dashboard **Info → Service** shows the running version and update status;
failed checks are shown as unavailable, not up to date. Checks time out after
five seconds, send no account credentials, and never install updates automatically.
Use **Check now** in the Info tab to check manually, even when live updates are
paused. Concurrent checks share one request, and results less than one minute
old are reused to avoid exhausting GitHub's API limit. Opening the tab or
refreshing dashboard data alone does not trigger an external check.

On standalone Linux and Windows release binaries (x64/ARM64), **Update and
restart** asks for confirmation, downloads the matching release asset, verifies
`SHA256SUMS` and the candidate's `--version`, and saves `<executable>.previous`
before replacing the executable. It then drains active requests, flushes runtime
state, and restarts with the same arguments, environment and working directory.
The dashboard reconnects and reloads the new frontend. The executable's directory
must be writable; no privilege escalation is attempted. An install failure keeps
the existing process running. A failure to launch the new executable restores the
backup and attempts to restart it; a failure occurring *after* successful launch
still requires manual recovery from `.previous` and is not automatically rolled back.

Docker, blue/green managed deployments, debug builds and unsupported platforms
only offer checks and update instructions, not executable replacement. Docker
must be updated by pulling an image and recreating the container externally.
Release binaries expose `kirolb --version` without opening the database or
creating configuration files.

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

## Codex CLI and Claude Code

`kirolb client` configures either CLI without changing the default Codex
profile or requiring a paid inference request for verification. Start the
gateway, export one of its data-plane keys, and run setup:

Automatic `setup` and `restore` mutate client files on Linux only. They fail
closed on macOS and Windows because those platforms do not provide the same
verified conditional commit path in this workflow. Read-only `diagnose` and
`status` remain available there; configure the clients manually using the
contracts below. This does not affect running the gateway on those platforms.

```bash
export KIROLB_API_KEY='your-gateway-key'
kirolb client setup all --base-url http://127.0.0.1:8000
```

Use `--api-key-stdin` to pipe the key instead of putting it in the environment.
Use `--api-key-env NAME` when Codex should read a different environment
variable, and `--model MODEL` to choose the Codex profile's initial model.
Never put the key on the command line: command arguments may be visible to
other local processes and shell history.

Setup first calls only `GET /health` and authenticated `GET /v1/models`. These
checks verify reachability, authentication, and model discovery without
generating tokens. Discovery must return at least one object with a nonempty
string model ID, so setup also detects a gateway with no serving account.
Health and discovery JSON responses are limited to 1 MiB each. Nothing is
written if either check fails. You can run the same read-only check separately:

```bash
kirolb client diagnose --base-url http://127.0.0.1:8000
```

Plaintext HTTP is accepted only for `localhost` and loopback IP addresses. If
`HTTP_PROXY`, `http_proxy`, `ALL_PROXY`, or `all_proxy` is set, setup also
requires an unambiguous `NO_PROXY`/`no_proxy` entry for that exact loopback host
in bare form (for example, `127.0.0.1` or `::1`) in both the setup environment
and the environment used to start the client. Port-qualified, bracketed,
wildcard, and case-variant entries are rejected because their behavior is not
consistent across clients. Preserved Claude Code `env` overrides are included
in this check. The read-only diagnostic always connects directly for plaintext
loopback, so its success alone does not prove that a generated client will
bypass a proxy. Use HTTPS for remote gateways so the bearer key is not sent in
cleartext.

For Codex, setup writes the dedicated profile
`$CODEX_HOME/kirolb.config.toml` (normally
`~/.codex/kirolb.config.toml`). Separate profile files require Codex 0.134.0 or
later. The profile reads the key from `KIROLB_API_KEY`; it does not contain the
key itself:

```bash
codex --profile kirolb
```

Using an isolated profile is deliberate: setup does not need to parse,
reformat, or merge the user's main TOML configuration.

For Claude Code, setup updates `~/.claude/settings.json` (or
`$CLAUDE_CONFIG_DIR/settings.json`) with `ANTHROPIC_BASE_URL`, a bearer token,
and gateway model discovery. Existing unrelated JSON settings are retained.
The file and the private restoration journal are written with owner-only
permissions on Unix. Confirm the active base URL and credential source with
`/status` inside Claude Code.

Inspect or undo setup at any time:

```bash
kirolb client status all
kirolb client restore all
```

Setup is repeatable and serialized across concurrent processes. Writes use a
same-directory temporary file and atomic rename. The private journal preserves
the exact prior bytes and Unix file mode, including the difference between a
missing file and an existing empty file. Restore is idempotent. If a configured
file changed after setup, restore refuses to overwrite it; reconcile that file
manually rather than deleting the journal. Symlinked configuration files and
configuration directories are rejected. Client files and restoration journals
are limited to 8 MiB each; setup rejects an oversized generated file or journal
before mutating a client file. An interrupted or conflicted operation can retain
recovery or `.kirolb-*.tmp` staging files that preserve original or concurrent
bytes needed for reconciliation. Review them before deletion and do not
bulk-delete these files by name.

### Compatibility limits

The `/v1/responses` endpoint is a translation facade, not persistent OpenAI
Responses storage. Codex must send each turn's complete conversation in
`input` with `store=false`; `previous_response_id` is rejected. The generated
profile therefore disables standalone web search and WebSocket transport,
which this gateway does not expose.

Claude Code uses the Anthropic Messages API and can discover models from
`/v1/models`. The gateway supports Messages streaming and token counting, but
it translates requests to Kiro rather than forwarding every current or future
Anthropic beta feature unchanged. Features that depend on direct Anthropic
services or a claude.ai identity, including cloud sessions and Remote Control,
are outside this setup. If a newly introduced Claude Code beta field is
rejected, try `CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS=1` and report the
incompatibility.

The generated fields and paths follow the current official client contracts:

- [Codex configuration reference](https://developers.openai.com/codex/config-reference)
- [Codex custom model providers](https://developers.openai.com/codex/config-advanced#custom-model-providers)
- [Connect Claude Code to an LLM gateway](https://code.claude.com/docs/en/llm-gateway-connect)
- [Claude Code gateway compatibility guide](https://code.claude.com/docs/en/llm-gateway-protocol)

## Release maintenance

Release-plz opens or updates a release PR after changes reach `main`. Review its
version and changelog, then merge it to approve a release. Use Conventional
Commits (`fix:`, `feat:`, and `!` / `BREAKING CHANGE:`) for useful release notes.
The baseline is `v0.2.0`; versions and tags are not reset. This is GitHub-only
distribution: `git_only = true` disables publishing to crates.io.

No personal access token or release secret is required. In Settings → Actions →
General → Workflow permissions, enable **Allow GitHub Actions to create and
approve pull requests**; keep the default token permissions read-only. The
release PR job grants its short-lived `GITHUB_TOKEN` Contents and Pull requests
write access, plus Actions write access to explicitly dispatch validation.
It does not approve or merge PRs.

After creating/updating a release PR, the job dispatches **Build** on that PR's
branch. This avoids depending on bot-generated PR events, which may require
approval. Dispatched builds check the branch commit and cannot publish Docker
images or releases, even when manually dispatched on a tag. Review the Build
checks on the release PR before merging. No dispatch is made when release-plz
reports no PR changes. The old `RELEASE_PLZ_TOKEN` secret is no longer read and
can be removed after verifying the token-free workflow.

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
