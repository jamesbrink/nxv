---
name: codex
description: Delegate tasks to Codex CLI (OpenAI's agentic coding tool) for alternative perspectives, code review, or parallel work. Use when asked to run Codex, get a second opinion, or leverage OpenAI models for specific tasks.
allowed-tools: Bash, Read
---

# Codex CLI Skill

Delegate tasks to [Codex CLI](https://github.com/openai/codex) for alternative model
perspectives or specialized capabilities.

## When to Use Codex

- **Second opinion**: Get an alternative perspective on implementation approaches
- **Code review**: Use `codex review` for automated code review
- **Parallel work**: Delegate subtasks while working on other things
- **Model comparison**: Compare OpenAI vs Anthropic model outputs for specific tasks

## Core Commands

### Non-Interactive Execution (Primary)

Use `codex exec` for non-interactive task execution:

```bash
codex exec "your prompt here"
```

**Common options:**

```bash
# Specify model
codex exec -m gpt-5.2-codex "describe the architecture"

# Full auto mode (workspace-write sandbox, minimal prompts)
codex exec --full-auto "refactor this function"

# Read-only sandbox (safer for analysis tasks)
codex exec -s read-only "analyze the codebase structure"

# Workspace-write sandbox (for modifications)
codex exec -s workspace-write "add tests for utils.rs"

# Set working directory
codex exec -C /path/to/project "your prompt"

# Enable web search
codex exec --search "what's the latest on this library"

# Attach images
codex exec -i screenshot.png "explain this UI"
```

### Code Review

Run non-interactive code review:

```bash
codex review
codex review --model gpt-5.2-codex
```

### Interactive Mode

Start interactive session (less common from Claude):

```bash
codex "your prompt"
codex --full-auto "your prompt"
```

## Sandbox Policies

| Policy | Description | Use Case |
|--------|-------------|----------|
| `read-only` | Read filesystem, no writes | Analysis, exploration |
| `workspace-write` | Write to workspace only | Safe modifications |
| `danger-full-access` | Full filesystem access | Trusted operations |

## Approval Policies

| Policy | Description |
|--------|-------------|
| `untrusted` | Only run trusted commands without approval |
| `on-failure` | Run all, ask on failure |
| `on-request` | Model decides when to ask |
| `never` | Never ask, return failures to model |

## Example Workflows

### Get a Second Opinion

```bash
codex exec -s read-only "Review the error handling in src/db/queries.rs and suggest improvements"
```

### Delegate a Subtask

```bash
codex exec --full-auto "Write unit tests for the bloom filter module in src/bloom.rs"
```

### Code Review

```bash
codex review
```

### Analysis Task

```bash
codex exec -s read-only "Analyze the performance characteristics of the indexer"
```

### Research with Web Search

```bash
codex exec --search "Find the latest best practices for SQLite FTS5 optimization"
```

## Working Directory

By default, Codex uses the current directory. To target a different project:

```bash
codex exec -C /path/to/other/project "your prompt"
```

## Model Selection

Specify models with `-m`:

```bash
codex exec -m gpt-5.2-codex "complex task"      # Default - Latest frontier agentic coding model
codex exec -m gpt-5.1-codex-max "reasoning"     # Deep and fast reasoning (flagship)
codex exec -m gpt-5.1-codex-mini "simple task"  # Cheaper, faster, but less capable
codex exec -m gpt-5.2 "general task"            # Latest frontier (non-codex optimized)
```

| Model | Description | Use Case |
|-------|-------------|----------|
| `gpt-5.2-codex` | Latest frontier agentic coding model | Default for most tasks |
| `gpt-5.1-codex-max` | Codex-optimized flagship for deep reasoning | Complex analysis, architecture |
| `gpt-5.1-codex-mini` | Cheaper, faster, less capable | Simple tasks, quick checks |
| `gpt-5.2` | Latest frontier model | General non-coding tasks |

## Tips

1. **Prefer `exec` over interactive** - Claude should use non-interactive mode
2. **Use read-only for analysis** - Safer when you just need information
3. **Capture output** - Redirect to file for large outputs: `codex exec "..." > output.md`
4. **Combine with Claude's context** - Pass relevant file contents in the prompt
5. **Check exit code** - `$?` indicates success (0) or failure

## Authentication

Codex requires OpenAI API authentication. Check status:

```bash
codex login
```

If not authenticated, inform the user they need to run `codex login`.

## Advanced Options

```bash
# Configuration overrides
codex exec -c model="gpt-5.1-codex-max" "reasoning task"
codex exec -c 'sandbox_permissions=["disk-full-read-access"]' "task"

# Multiple additional directories
codex exec --add-dir /path/to/shared/lib "task needing shared code"

# Profile from config.toml
codex exec -p my-profile "task"
```

## Output Handling

For tasks generating diffs or patches:

```bash
# Apply changes from last Codex run
codex apply

# Or apply with shorthand
codex a
```

## See Also

- `codex --help` - Full CLI reference
- `codex exec --help` - Exec-specific options
- `codex review --help` - Review options
