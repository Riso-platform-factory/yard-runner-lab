# Ponytail Plugin Profile (`ponytail`)

Verified project-scoped linking blueprint for `~/.agents/ponytail`
(`https://github.com/DietrichGebert/ponytail`), avoiding global installation.

## Plugin Analysis

- **Cache path**: `~/.agents/ponytail`
- **Manifests**:
  - `plugin.json` (`{"name": "ponytail"}` — Antigravity plugin marker)
  - `.claude-plugin/plugin.json` & `.claude-plugin/marketplace.json`
  - `.codex-plugin/plugin.json`
  - `gemini-extension.json`
- **Skills** (`skills/<name>/SKILL.md`, 6 skills total):
  - `ponytail`
  - `ponytail-audit`
  - `ponytail-debt`
  - `ponytail-gain`
  - `ponytail-help`
  - `ponytail-review`
  - *Ignore* `skills/plugin.json` (metadata file, not a skill directory).
- **Hooks** (`hooks/claude-codex-hooks.json`):
  - Events: `SessionStart` (`matcher: "startup|resume|clear|compact"`), `SubagentStart`, `UserPromptSubmit`.
  - Scripts (require `node` on `PATH`):
    - `hooks/ponytail-activate.js`
    - `hooks/ponytail-subagent.js`
    - `hooks/ponytail-mode-tracker.js`
  - Runtime env detection (`hooks/ponytail-runtime.js`):
    - Reads `CLAUDE_PLUGIN_ROOT` (must be set).
    - Uses `CLAUDE_PROJECT_DIR` for per-project Claude state under `~/.claude/ponytail-modes/<sha256>`.
    - Sets `isCodex = true` when `PLUGIN_DATA` is non-empty (stores state in `$PLUGIN_DATA` and outputs Codex `hookSpecificOutput` JSON).
  - **Antigravity**: No Antigravity `hooks.json` at plugin root (`agy plugin validate` reports `skills 6 · hooks 0 · rules 0 · mcpServers 0`). Do **not** wire `claude-codex-hooks.json` into `.agents/hooks.json`.
- **Rules**:
  - Ships `AGENTS.md` and `.agents/rules/ponytail.md` (always-on rule).
  - If the target project already has `AGENTS.md` / `CLAUDE.md`, **skip** linking rules so project rules are not overridden; use skills + hooks instead.

## Project-Scope Copy, Symlinks & Wiring

### 1. Copy Skills into `.agents/skills/` (Antigravity + Codex) & Symlink `.claude/`

```bash
mkdir -p .agents/skills .claude/plugins .claude/skills .codex

# Copy only the 6 skill directories into .agents/skills (no .agents/plugins bloat)
for name in ponytail ponytail-audit ponytail-debt ponytail-gain ponytail-help ponytail-review; do
  rsync -a --delete --exclude='.git' "$HOME/.agents/ponytail/skills/$name/" ".agents/skills/$name/"
  ln -sfn "../../.agents/skills/$name" ".claude/skills/$name"
done

# Claude Code plugin anchor for hooks
ln -sfn "$HOME/.agents/ponytail" .claude/plugins/ponytail
```

### 2. Claude Code Hooks (`.claude/settings.local.json`)

Merge into `"hooks"` in `.claude/settings.local.json`:

```json
{
  "hooks": {
    "SessionStart": [
      {
        "matcher": "startup|resume|clear|compact",
        "hooks": [
          {
            "type": "command",
            "command": "CLAUDE_PLUGIN_ROOT=\"$CLAUDE_PROJECT_DIR/.claude/plugins/ponytail\" node \"$CLAUDE_PROJECT_DIR/.claude/plugins/ponytail/hooks/ponytail-activate.js\"",
            "timeout": 15,
            "statusMessage": "Loading ponytail mode..."
          }
        ]
      }
    ],
    "SubagentStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "CLAUDE_PLUGIN_ROOT=\"$CLAUDE_PROJECT_DIR/.claude/plugins/ponytail\" node \"$CLAUDE_PROJECT_DIR/.claude/plugins/ponytail/hooks/ponytail-subagent.js\"",
            "timeout": 15,
            "statusMessage": "Loading ponytail mode..."
          }
        ]
      }
    ],
    "UserPromptSubmit": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "CLAUDE_PLUGIN_ROOT=\"$CLAUDE_PROJECT_DIR/.claude/plugins/ponytail\" node \"$CLAUDE_PROJECT_DIR/.claude/plugins/ponytail/hooks/ponytail-mode-tracker.js\"",
            "timeout": 15,
            "statusMessage": "Tracking ponytail mode..."
          }
        ]
      }
    ]
  }
}
```

### 3. Codex Hooks (`.codex/hooks.json`)

Merge into `"hooks"` in `.codex/hooks.json` (note `PLUGIN_DATA` keeps state project-local under `.codex/plugin-data/ponytail`):

```json
{
  "hooks": {
    "SessionStart": [
      {
        "matcher": "startup|resume|clear|compact",
        "hooks": [
          {
            "type": "command",
            "command": "CLAUDE_PLUGIN_ROOT=\"$HOME/.agents/ponytail\" PLUGIN_DATA=\"$PWD/.codex/plugin-data/ponytail\" node \"$HOME/.agents/ponytail/hooks/ponytail-activate.js\"",
            "timeout": 15,
            "statusMessage": "Loading ponytail mode..."
          }
        ]
      }
    ],
    "SubagentStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "CLAUDE_PLUGIN_ROOT=\"$HOME/.agents/ponytail\" PLUGIN_DATA=\"$PWD/.codex/plugin-data/ponytail\" node \"$HOME/.agents/ponytail/hooks/ponytail-subagent.js\"",
            "timeout": 15,
            "statusMessage": "Loading ponytail mode..."
          }
        ]
      }
    ],
    "UserPromptSubmit": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "CLAUDE_PLUGIN_ROOT=\"$HOME/.agents/ponytail\" PLUGIN_DATA=\"$PWD/.codex/plugin-data/ponytail\" node \"$HOME/.agents/ponytail/hooks/ponytail-mode-tracker.js\"",
            "timeout": 15,
            "statusMessage": "Tracking ponytail mode..."
          }
        ]
      }
    ]
  }
}
```

### 4. Local Git Exclude (`.git/info/exclude`)

```gitignore
.agents/
.claude/plugins/
.claude/skills/
.claude/settings.local.json
.codex/
```

## Verification & Unlink

- Validate Claude skills: `claude plugin validate .claude/skills`
- Dry-run Claude hook: `CLAUDE_PROJECT_DIR="$PWD" CLAUDE_PLUGIN_ROOT="$PWD/.claude/plugins/ponytail" node "$PWD/.claude/plugins/ponytail/hooks/ponytail-activate.js" </dev/null`
- Dry-run Codex hook: `CLAUDE_PLUGIN_ROOT="$HOME/.agents/ponytail" PLUGIN_DATA="$PWD/.codex/plugin-data/ponytail" node "$HOME/.agents/ponytail/hooks/ponytail-activate.js" </dev/null`
- Unlink from project (leaves `~/.agents/ponytail` intact):
  ```bash
  rm -rf .agents/skills/ponytail .agents/skills/ponytail-* \
         .claude/plugins/ponytail .claude/skills/ponytail .claude/skills/ponytail-* \
         .codex/plugin-data/ponytail
  ```
  (And remove the `ponytail` hook entries from `.claude/settings.local.json` and `.codex/hooks.json`.)
