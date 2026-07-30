# Agent Skill

nxv ships an [Agent Skills](https://agentskills.io)-standard skill that lets AI
coding agents — Claude Code, OpenAI Codex CLI, Pi, OpenClaw, GitHub Copilot CLI,
Cursor, Gemini CLI, Amp, Goose, and anything else that reads `SKILL.md` — run
nxv commands on your behalf without extra setup. The nxv binary embeds the skill
and installs it for you.

## What is a skill?

A skill is a `SKILL.md` file (with optional supporting files) that teaches an
agent how to use a tool. When the skill is loaded, the agent knows every
subcommand, flag, JSON output shape, and HTTP endpoint for nxv. It can answer
questions like "which nixpkgs commit shipped python 2.7?" or "give me the
`nix shell` command for nodejs 15" by running the right `nxv` invocation (or
hitting the public API) and interpreting the result.

## Installing the skill

The `nxv skill` subcommand generates and installs the skill from a single
embedded template:

```bash
# Install user-wide for one agent
nxv skill install codex

# Explicitly install for agents detected on this machine
nxv skill install --detected

# Map detected agents to their project-level paths
nxv skill install --detected --project

# Install for specific agents only
nxv skill install claude codex

# Install for every supported agent, detected or not
nxv skill install --all

# See supported agents, their paths, and what's installed where
nxv skill list
```

Installation requires one explicit target mode: agent names, `--detected`, or
`--all`. Supplying no target is an error and writes nothing. `--detected` checks
agent configuration under your home directory; with `--project` or `--dir`,
those agents are mapped to their project-level paths. No detected agents is an
error rather than an implicit generic install. Per agent, the skill lands in
`<skills dir>/nxv/SKILL.md`:

| Agent      | User-wide                 | Project-level     |
| ---------- | ------------------------- | ----------------- |
| `claude`   | `~/.claude/skills/`       | `.claude/skills/` |
| `codex`    | `~/.codex/skills/`        | `.agents/skills/` |
| `pi`       | `~/.pi/agent/skills/`     | `.pi/skills/`     |
| `openclaw` | `~/.openclaw/skills/`     | `.agents/skills/` |
| `copilot`  | `~/.copilot/skills/`      | `.github/skills/` |
| `cursor`   | `~/.cursor/skills/`       | `.agents/skills/` |
| `gemini`   | `~/.gemini/skills/`       | `.agents/skills/` |
| `amp`      | `~/.config/amp/skills/`   | `.agents/skills/` |
| `goose`    | `~/.config/goose/skills/` | `.agents/skills/` |
| `agents`   | `~/.agents/skills/`       | `.agents/skills/` |

The `agents` target is the generic cross-agent directory from the Agent Skills
standard. Select it explicitly with `nxv skill install agents`.

The table shows each agent's primary directory — the one
`nxv skill install <agent>` writes to. Several agents read additional locations:
Copilot reads `.github/skills/`, `.claude/skills/`, or `.agents/skills/` in a
repository, and Pi reads `.agents/skills/` as well as `.pi/skills/`. Agents that
share a selected project path are deduplicated into one write.

To remove installed skills:

```bash
nxv skill uninstall            # Remove from every user-wide agent path
nxv skill uninstall --project  # Remove project-level installs
```

### Manual install (no nxv binary)

Fetch the canonical copy straight from the repository:

```bash
mkdir -p ~/.claude/skills/nxv
curl -sL https://raw.githubusercontent.com/utensils/nxv/main/.claude/skills/nxv/SKILL.md \
  -o ~/.claude/skills/nxv/SKILL.md
```

## Using the skill

Once installed, agents that support slash-command invocation can call it
directly:

```
/nxv search python 2.7
/nxv search python 2.7.3 --all-depths
/nxv run python 2.7
/nxv run python 3.11 --with nodejs@20 --with jq
/nxv info python311 3.11.4
/nxv history nodejs-15_x
```

Or just ask naturally — the agent loads the skill automatically when your
question matches its description:

> "Which nixpkgs commit had python 2.7?"
>
> "Give me the `nix shell` command for nodejs 15.14."
>
> "Open a shell with Python 3.11 and Node.js 20."
>
> "When was ruby 2.6 last in nixpkgs?"

You don't need a local index for the skill to be useful — agents can hit the
public API at `https://nxv.urandom.io` directly, or you can set
`NXV_API_URL=https://nxv.urandom.io` in your environment so the CLI uses the
hosted instance transparently.

## For agents

The skill is designed so autonomous agents can extract structured data reliably.
The `search`, `info`, and `history` subcommands support `--format json`, and
every data-returning HTTP API response is wrapped in a stable `{ "data": ... }`
envelope (plus `meta` for paginated lists; only the operational `/health` and
`/metrics` endpoints are unwrapped). Agents should:

1. Run `nxv <subcommand> --format json` (CLI) or hit `/api/v1/...` (HTTP) for
   machine-readable output.
2. Pipe to `jq` (or parse in-process) for the specific field they need.
3. Never rely on the human-readable table output — column widths and formatting
   are terminal-dependent.

For a version-qualified prefix search, nxv searches the shallowest matching
attribute-path tier first. If no version matches, API consumers should inspect
`meta.resolution.suggestions` and `deeper_matches_available`; pass
`all_depths=true` only when nested package-set matches are intentional.
Successful CLI JSON searches return an array; an empty miss emits no stdout,
with the miss explanation written to stderr.

For an interactive environment, agents can skip command construction entirely:

```bash
nxv run python 2.7
nxv run python 3.11 --with nodejs@20 --with jq
```

`run` resolves every query first, prefers an exact attribute before falling
back to deterministic search relevance, and launches one pinned shell. It also
handles pre-flake revisions and known insecure packages using the appropriate
Nix invocation. On Apple Silicon, pre-flake shells use `x86_64-darwin` and
require Rosetta.

Example agent pattern — generate a `nix shell` invocation for a specific version
directly from the public API:

```bash
curl -s "https://nxv.urandom.io/api/v1/packages/python/versions/2.7.18/first" | \
  jq -r '.data | "nix shell nixpkgs/\(.first_commit_hash | .[0:7])#\(.attribute_path)"'
```

Or via the CLI against any backend:

```bash
nxv search nodejs-15_x 15 --exact --format json | \
  jq -r '.[0] | "nix shell nixpkgs/\(.first_commit_hash | .[0:7])#\(.attribute_path)"'
```

For old nixpkgs commits, modern Nix may reject a retired flake `edition`, and
pre-Apple-Silicon revisions may fail to evaluate packages as `aarch64-darwin`.
Use a classic import with the full hash and exact returned attribute path,
evaluating as `x86_64-darwin` when Rosetta is available:

```bash
nix shell --impure --expr '
  (import (builtins.fetchTarball
    "https://github.com/NixOS/nixpkgs/archive/<full-hash>.tar.gz")
    { system = "x86_64-darwin"; }).ruby
' --command ruby --version
```

The returned `platforms` values are package metadata, not proof that a
historical revision evaluates on that system or has a Hydra-cached binary.

## Keeping the skill up to date

The installed skill is byte-identical to the template embedded in your nxv
binary, so refreshing it is just upgrading nxv and reinstalling:

```bash
nxv update                  # Get the latest nxv application
nxv skill install codex     # Rewrite one explicit installation
nxv skill install --detected # Or explicitly refresh detected agents
```

## Skill source

The canonical template lives at
[`src/skill/SKILL.md`](https://github.com/utensils/nxv/blob/main/src/skill/SKILL.md)
in the repository; the checked-in copies under `.claude/skills/` and
`.agents/skills/` are generated from it.
