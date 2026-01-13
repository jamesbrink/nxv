---
description: Run a task using Codex CLI with the provided prompt
allowed-tools: Bash, Read, AskUserQuestion
---

# Codex Command

Execute a task using Codex CLI (OpenAI's agentic coding tool).

## Usage

```text
/codex <prompt>
/codex review
/codex <prompt> --model gpt-4o
```

## Execution

When invoked, run the appropriate Codex command based on the user's input:

### If prompt is "review" or starts with "review"

Run code review:

```bash
codex review
```

### For all other prompts

Run non-interactive execution with appropriate sandbox:

1. **Analysis/exploration tasks** (ask, explain, analyze, describe, find):

   ```bash
   codex exec -s read-only "<user prompt>"
   ```

2. **Modification tasks** (add, create, refactor, fix, update, implement):

   ```bash
   codex exec --full-auto "<user prompt>"
   ```

3. **If unclear**, ask the user:
   - Read-only (safer, for analysis)
   - Full-auto (for modifications)

## Model Selection

If the user specifies a model with `--model` or `-m`, pass it through:

```bash
codex exec -m <model> "<prompt>"
```

Default model is whatever Codex is configured to use (typically gpt-4o).

## Working Directory

Run Codex in the current project directory. If the user specifies a different
directory with `-C` or `--cd`, pass it through.

## Output

1. Run the Codex command and capture output
2. Present the results to the user
3. If Codex suggests changes, inform the user they can apply with `codex apply`

## Error Handling

- If Codex is not installed, inform the user to install it
- If authentication fails, suggest running `codex login`
- If the command fails, show the error and suggest alternatives

## Examples

User: `/codex explain the indexer architecture`
Action: `codex exec -s read-only "explain the indexer architecture"`

User: `/codex add unit tests for bloom.rs`
Action: `codex exec --full-auto "add unit tests for bloom.rs"`

User: `/codex review`
Action: `codex review`

User: `/codex --model o3 analyze performance`
Action: `codex exec -m o3 -s read-only "analyze performance"`
