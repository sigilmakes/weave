# Weave

Weave is a terminal agent for coding and research. The model works through a
persistent Python REPL. Sessions run in a background daemon and can be resumed
after the client exits.

The CLI is `wv`. This is the downstream Rust repository; Mnemosyne is the
source of truth and GitHub is a mirror.

- Repository: <https://mnemosyne.sigilzero.dev/sigilzero/weave>
- Issues: <https://mnemosyne.sigilzero.dev/sigilzero/weave/issues>
- Version: 0.10.0
- License: MIT

## Build

Build the application with its Python runtime, skills, and catalogs:

```bash
nix build
./result/bin/wv --version
```

`nix develop` provides the pinned development tools. On Willow's NixOS fleet,
`/config` owns the installation; do not use upstream installers, global npm
packages, or self-update for a Nix-owned installation.

## Run

Start Weave from the directory you want it to work in:

```bash
cd /path/to/project
wv
```

On first launch, run `/login` to choose a subscription or API-key provider.
Weave works in the current directory and can run commands and modify files there.
Use a disposable clone, clean worktree, or another checkpoint you can inspect and
restore.

> [!WARNING]
> Weave executes model-generated Python and project commands with your user
> permissions. Worker and kernel processes isolate session lifecycles and support
> recovery; they are **not** a security sandbox. Review changes and use trusted
> repositories, instructions, and skills only. Run untrusted code or instructions
> in an external sandbox or restricted environment.

Useful commands:

```bash
wv agents                   # Browse running, idle, and saved sessions
wv attach <agent>           # Reattach to a running session
wv --resume [path|id]       # Browse sessions or resume one directly
wv status                   # Inspect background service state
wv doctor [--fix]           # Inspect or repair background services
wv update [--force]         # Update Weave
wv shutdown [--force]       # Stop every agent, worker, and background service
```

## Configuration

Weave trusts your operating system's trust store together with the bundled
Mozilla roots. For a server behind a custom CA, point `NODE_EXTRA_CA_CERTS` at a
PEM bundle:

```bash
export NODE_EXTRA_CA_CERTS=/path/to/your-ca.pem
```

Each transport reads the variable once when its first client is initialized. The
background daemon keeps the environment it started with, so run `wv shutdown`
after changing it.

Clipboard copy uses the local clipboard tool when available. In tmux it forwards
through the attached client; without tmux it falls back to OSC 52 over SSH.
Delivery cannot always be confirmed, so the UI reports it as unconfirmed.

Connect Slack from `/mcp` or with `/mcp login slack`. See the
[Slack connector guide](docs/slack-mcp.md) for setup and troubleshooting.

## Development

Read [AGENTS.md](AGENTS.md) before contributing. Before integration, `make check`
runs the workspace formatting, clippy, test, and hook gates.
`nix flake check --no-update-lock-file` adds the Nix package, privacy, and audited
runtime checks; it does not replace the full Rust gate.

For credential-isolated validation with a prepared real kernel, enter
`nix develop --no-update-lock-file`, then run:

```bash
python3 -B scripts/check_workspace.py
```

This can download locked build and Python dependencies. It never needs live
provider or MCP credentials. It uses private test state and one reusable Cargo
target. See [CONTRIBUTING.md](CONTRIBUTING.md) and [GLOSSARY.md](GLOSSARY.md) for
contribution and domain guidance.

## Context compaction

The builtin rolling context strategy tries server-side compaction, then standard
text summarization. No compaction route settings are required. The server attempt
uses the **resolved** `openai-responses` provider, endpoint and model; other transports
skip it. Codex native archive replay is not supported, so those transports fall
through to standard summarization. Only an explicit unsupported-capability response
(or a local input-budget decline) advances the chain. Authentication, route errors,
malformed responses, cancellation and uncertain network outcomes stop the operation.
Provider-native
archives stay bound to that exact route and model. The server request inherits
resolved model/provider authentication headers; its explicit API key remains the
bearer credential. Canonical history is retained independently of these context
archives.

To replace the chain, set `plugins.default_context.config.compactors` to an ordered
array of `plugin/compactor` routes (an empty array disables compaction attempts).
For example, `["standard_compaction/standard"]` selects text summarization only.
Disabled compactor plugins are skipped. Set `plugins.server_compaction.config.route`
to `null` to disable the server probe, or supply an exact `{provider, endpoint, model}`
route override; a mismatched override is an error. Custom context strategies keep
control of their own chains.


### Automatic compaction settings

`/settings` exposes the global automatic compaction percentage in five-point steps,
with a 95% default. A saved off-grid percentage stays selectable. The effective
trigger also reserves room for retained history and summaries; the model's input
and requested-output ceiling always takes priority. Manual compaction budgets are
unchanged. The underlying `compaction.thresholdPercent` setting accepts 1–100,
including fractional values; project settings can override the global setting.

For per-model early compaction, use the existing provider-scoped
`modelOverrides.<id>.contextWindow` option in `models.json`, for example:

```json
{
  "providers": {
    "prime-inference": {
      "modelOverrides": {
        "your-model-id": { "contextWindow": 524288 }
      }
    }
  }
}
```

Use the actual model ID for your provider. This sets that model's effective context
window; it does not change every model's global compaction percentage.
