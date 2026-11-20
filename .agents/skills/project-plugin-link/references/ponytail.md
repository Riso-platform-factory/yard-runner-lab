# Ponytail Plugin Profile (`ponytail`)

Verified project-scoped Model A blueprint for `~/.agents/ponytail`
(`https://github.com/DietrichGebert/ponytail`), avoiding global installation and repo clutter.

## Plugin Analysis

- **Cache path**: `~/.agents/ponytail`
- **Skills** (`skills/<name>/SKILL.md`, 6 skills total):
  - `ponytail`
  - `ponytail-audit`
  - `ponytail-debt`
  - `ponytail-gain`
  - `ponytail-help`
  - `ponytail-review`
  - *Skip* `skills/plugin.json` (metadata file, not a skill directory).
- **Hooks** (`hooks/claude-codex-hooks.json`):
  - Events: `SessionStart` (`matcher: "startup|resume|clear|compact"`), `SubagentStart`, `UserPromptSubmit`.
  - Runtime scripts required (require `node` on `PATH`):
    - Entrypoints: `hooks/ponytail-activate.js`, `hooks/ponytail-subagent.js`, `hooks/ponytail-mode-tracker.js`
    - Internal helpers required/spawned by entrypoints: `hooks/ponytail-runtime.js`, `hooks/ponytail-config.js`, `hooks/ponytail-instructions.js`, `hooks/ponytail-map.js`, `hooks/ponytail-band-import.js`, `hooks/ponytail-statusline.sh`
  - Relative path dependency: `hooks/ponytail-instructions.js` reads `path.join(__dirname, '..', 'skills', 'ponytail', 'SKILL.md')`, which resolves naturally inside `.agents/vendor/ponytail/`.
  - Runtime env detection (`hooks/ponytail-runtime.js`):
    - Reads `CLAUDE_PLUGIN_ROOT` (set to `.agents/vendor/ponytail`).
    - Sets `isCodex = true` when `PLUGIN_DATA` is non-empty (stores state in `$PWD/.codex/plugin-data/ponytail` and outputs Codex `hookSpecificOutput` JSON).
  - **Antigravity**: No Antigravity `hooks.json` at plugin root. Do **not** wire `claude-codex-hooks.json` into `.agents/hooks.json`.
- **Pruned repo clutter (do NOT copy into workspace)**:
  - `.git/`, `.github/`, `tests/`, `benchmarks/`, `docs/`, `examples/`, `assets/`, `commands/`, `i18n/`, `pi-extension/`, `scripts/`, `hooks/band/`, `hooks/*-hooks.json`, `AGENTS.md`, `.agents/rules/`, `.cursor/`, `.windsurf/`, `.qoder/`, `.kiro/`, `.opencode/`, `.openclaw/`, `README.md`, `INSTALL.md`, `package.json`, `plugin.json`, `plugin.yaml`, `gemini-extension.json`.
- **Rules**:
  - Ships `AGENTS.md` and `.agents/rules/ponytail.md` (always-on rule).
  - If the target project already has `AGENTS.md` / `CLAUDE.md`, **skip** copying rules so project rules are not overridden; use skills + hooks instead.

## Project-Scope Materialization & Wiring

### 1. Pruned Copy into `.agents/vendor/ponytail/` & Intra-Workspace Symlinks

```bash
mkdir -p .agents/vendor/ponytail/skills .agents/vendor/ponytail/hooks .agents/skills .claude/skills .codex

# 6 skills -> .agents/vendor/ponytail/skills/<name> + intra-workspace symlinks
for name in ponytail ponytail-audit ponytail-debt ponytail-gain ponytail-help ponytail-review; do
  rsync -a --delete --exclude='.git' "$HOME/.agents/ponytail/skills/$name/" ".agents/vendor/ponytail/skills/$name/"
  ln -sfn "../vendor/ponytail/skills/$name" ".agents/skills/$name"
  ln -sfn "../../.agents/vendor/ponytail/skills/$name" ".claude/skills/$name"
done

# Runtime hook scripts only (preserving ../skills/ layout)
cp -p "$HOME/.agents/ponytail"/hooks/ponytail-*.js \
      "$HOME/.agents/ponytail"/hooks/ponytail-statusline.sh \
      .agents/vendor/ponytail/hooks/
```

### 2. Claude Code Hooks (`.claude/settings.local.json`)

Merge into `"hooks"` in `.claude/settings.local.json` (no `.claude/plugins/` symlink needed):

```json
{
  "hooks": {
    "SessionStart": [
      {
        "matcher": "startup|resume|clear|compact",
        "hooks": [
          {
            "type": "command",
            "command": "CLAUDE_PLUGIN_ROOT=\"$CLAUDE_PROJECT_DIR/.agents/vendor/ponytail\" node \"$CLAUDE_PROJECT_DIR/.agents/vendor/ponytail/hooks/ponytail-activate.js\"",
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
            "command": "CLAUDE_PLUGIN_ROOT=\"$CLAUDE_PROJECT_DIR/.agents/vendor/ponytail\" node \"$CLAUDE_PROJECT_DIR/.agents/vendor/ponytail/hooks/ponytail-subagent.js\"",
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
            "command": "CLAUDE_PLUGIN_ROOT=\"$CLAUDE_PROJECT_DIR/.agents/vendor/ponytail\" node \"$CLAUDE_PROJECT_DIR/.agents/vendor/ponytail/hooks/ponytail-mode-tracker.js\"",
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
            "command": "CLAUDE_PLUGIN_ROOT=\"$PWD/.agents/vendor/ponytail\" PLUGIN_DATA=\"$PWD/.codex/plugin-data/ponytail\" node \"$PWD/.agents/vendor/ponytail/hooks/ponytail-activate.js\"",
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
            "command": "CLAUDE_PLUGIN_ROOT=\"$PWD/.agents/vendor/ponytail\" PLUGIN_DATA=\"$PWD/.codex/plugin-data/ponytail\" node \"$PWD/.agents/vendor/ponytail/hooks/ponytail-subagent.js\"",
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
            "command": "CLAUDE_PLUGIN_ROOT=\"$PWD/.agents/vendor/ponytail\" PLUGIN_DATA=\"$PWD/.codex/plugin-data/ponytail\" node \"$PWD/.agents/vendor/ponytail/hooks/ponytail-mode-tracker.js\"",
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
.claude/skills/
.claude/settings.local.json
.codex/
```

## Verification & Unlink

- Validate Claude skills: `claude plugin validate .claude/skills`
- Dry-run Claude hook: `CLAUDE_PROJECT_DIR="$PWD" CLAUDE_PLUGIN_ROOT="$PWD/.agents/vendor/ponytail" node "$PWD/.agents/vendor/ponytail/hooks/ponytail-activate.js" </dev/null`
- Dry-run Codex hook: `CLAUDE_PLUGIN_ROOT="$PWD/.agents/vendor/ponytail" PLUGIN_DATA="$PWD/.codex/plugin-data/ponytail" node "$PWD/.agents/vendor/ponytail/hooks/ponytail-activate.js" </dev/null`
- Unlink from project (leaves `~/.agents/ponytail` intact):
  ```bash
  rm -rf .agents/vendor/ponytail .agents/skills/ponytail .agents/skills/ponytail-* \
         .claude/skills/ponytail .claude/skills/ponytail-* \
         .codex/plugin-data/ponytail
  ```
  (And remove the `ponytail` hook entries from `.claude/settings.local.json` and `.codex/hooks.json`.)
