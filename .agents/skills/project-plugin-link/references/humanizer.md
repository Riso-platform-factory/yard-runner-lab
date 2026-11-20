# Humanizer Plugin/Skill Profile (`humanizer`)

Verified project-scoped Model A blueprint for `~/.agents/humanizer`
(`https://github.com/blader/humanizer`), avoiding global installation and repo clutter.

## Analysis

- **Cache path**: `~/.agents/humanizer`
- **Layout type**: **Skill-at-root** (`SKILL.md` is directly at the repository root, not inside `skills/`).
- **Runtime files actually needed**:
  - `SKILL.md` (name: `humanizer`, version: `4.0.0` — self-contained, does not reference `scripts/` or other repo files)
  - `agents/openai.yaml` (Codex UI metadata & default prompt)
- **Pruned repo clutter (do NOT copy into workspace)**:
  - `AGENTS.md` (contributor guide for editing Humanizer itself)
  - `.git/`, `.github/`, `.claude-plugin/`, `.cursor-plugin/`, `README.md`, `CHANGELOG.md`, `LICENSE`, `package.json`, `scripts/validate-package.py`
- **Hooks / MCP**: None.

## Project-Scope Materialization & Wiring

### 1. Pruned Copy into `.agents/vendor/humanizer/` & Intra-Workspace Symlinks

```bash
mkdir -p .agents/vendor/humanizer/skills/humanizer/agents .agents/skills .claude/skills

cp -p "$HOME/.agents/humanizer/SKILL.md" .agents/vendor/humanizer/skills/humanizer/SKILL.md
cp -p "$HOME/.agents/humanizer/agents/openai.yaml" .agents/vendor/humanizer/skills/humanizer/agents/openai.yaml

ln -sfn ../vendor/humanizer/skills/humanizer .agents/skills/humanizer
ln -sfn ../../.agents/vendor/humanizer/skills/humanizer .claude/skills/humanizer
```

- **Antigravity (CLI + IDE)**: Loads `.agents/skills/humanizer/SKILL.md` via intra-workspace symlink to `.agents/vendor/humanizer/skills/humanizer`.
- **Codex**: Loads `.agents/skills/humanizer/SKILL.md` and `agents/openai.yaml`.
- **Claude Code**: Loads `.claude/skills/humanizer/SKILL.md` via intra-workspace symlink (no `settings.local.json` hook edits needed).

### 2. Local Git Exclude (`.git/info/exclude`)

```gitignore
.agents/
.claude/skills/
```

## Verification & Unlink

- Verify vendor files & symlinks resolve:
  `test -f .agents/skills/humanizer/SKILL.md && test -f .claude/skills/humanizer/SKILL.md`
- Validate Claude skills dir: `claude plugin validate .claude/skills`
- Unlink from project (leaves `~/.agents/humanizer` intact):
  ```bash
  rm -rf .agents/vendor/humanizer .agents/skills/humanizer .claude/skills/humanizer
  ```
