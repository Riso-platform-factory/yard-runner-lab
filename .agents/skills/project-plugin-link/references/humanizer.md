# Humanizer Plugin/Skill Profile (`humanizer`)

Verified project-scoped linking blueprint for `~/.agents/humanizer`
(`https://github.com/blader/humanizer`), avoiding global installation.

## Analysis

- **Cache path**: `~/.agents/humanizer`
- **Layout type**: **Skill-at-root** (`SKILL.md` is directly at the repository root, not inside `skills/`).
- **Manifests**:
  - `.claude-plugin/plugin.json` (`"skills": ["./"]`, no hooks, no MCP) & `.claude-plugin/marketplace.json`
  - `.cursor-plugin/plugin.json`
  - `package.json` (`pi` package)
  - No root `plugin.json` / `.antigravity-plugin/plugin.json` / `.gemini-plugin/plugin.json`.
- **Skills**:
  - `SKILL.md` (name: `humanizer`, version: `4.0.0`)
  - `agents/openai.yaml` (Codex UI metadata & default prompt)
- **Hooks / MCP**: None.
- **Rules**: `AGENTS.md` at root is a contributor guide for editing Humanizer itself — do **not** link it as a project rule.

## Project-Scope Copy & Symlinks

### 1. Copy for Antigravity + Codex (`.agents/`) & Symlink for Claude Code (`.claude/`)

Because `SKILL.md` and `agents/openai.yaml` sit at the repository root (and there is no root Antigravity `plugin.json`), copy the repo (excluding `.git`) into `.agents/skills/humanizer`, then symlink `.claude/skills/humanizer` from that copy:

```bash
mkdir -p .agents/skills .claude/skills

rsync -a --delete --exclude='.git' "$HOME/.agents/humanizer/" .agents/skills/humanizer/
ln -sfn ../../.agents/skills/humanizer .claude/skills/humanizer
```

- **Antigravity**: Loads `.agents/skills/humanizer/SKILL.md` (real directory).
- **Codex**: Loads `.agents/skills/humanizer/SKILL.md` and `agents/openai.yaml`.
- **Claude Code**: Loads `.claude/skills/humanizer/SKILL.md` via symlink to `.agents/skills/humanizer` (no `settings.local.json` hook edits needed).

### 2. Local Git Exclude (`.git/info/exclude`)

```gitignore
.agents/
.claude/skills/
```

## Verification & Unlink

- Verify copy & symlink resolve:
  `test -f .agents/skills/humanizer/SKILL.md && test -f .claude/skills/humanizer/SKILL.md`
- Validate Claude skills dir: `claude plugin validate .claude/skills`
- Unlink from project (leaves `~/.agents/humanizer` intact):
  ```bash
  rm -rf .agents/skills/humanizer .claude/skills/humanizer
  ```
