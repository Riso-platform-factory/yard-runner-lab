---
name: project-plugin-link
description: >-
  Clone a plugin repository into ~/.agents/<name> (if not already present),
  analyze its structure (skills and hooks/scripts), and install a pruned,
  self-contained project-scoped runtime into .agents/vendor/<name>/ with
  intra-workspace symlinks for Antigravity, Codex, and Claude Code. Use when
  the user asks to attach, link, or test a plugin for the current project
  without installing it globally across all projects.
---

# Project-Scoped Plugin Linker (Model A: Self-Contained `.agents/vendor/`)

Materialize a minimal, self-contained runtime of a plugin inside the **current
repository only** from a shared upstream cache at `~/.agents/<plugin-name>`,
wiring **Antigravity (IDE + CLI)**, **Codex**, and **Claude Code** without
touching global AI configs (`~/.claude`, `~/.codex`, `~/.gemini/config`).

## Architecture Rationale

- **Upstream cache (`~/.agents/<plugin-name>`)**: holds the full git clone for
  updates across projects, never referenced directly at runtime.
- **Pruned in-project store (`.agents/vendor/<plugin-name>/`)**: holds **only**
  the 2 runtime essentials — `skills/` (whitelisted skill files) and `hooks/`
  (runtime hook scripts) — preserving sibling `hooks/` ↔ `../skills/` paths
  while keeping repo bloat (`tests/`, `benchmarks/`, `.git/`, `.github/`,
  unrelated `AGENTS.md`, `README.md`, other IDE manifests) out of the project.
  Using `.agents/vendor/` (not `.agents/plugins/`) prevents Antigravity from
  auto-scanning raw repos or loading conflicting rules.
- **Intra-workspace symlinks**: Antigravity (CLI + IDE), Codex, and Claude Code
  all resolve symlinks whose targets stay **inside the workspace root**, and
  hook scripts inside `.agents/vendor/` run cleanly even under terminal sandboxes
  that block `$HOME/.agents/`.

## Workflow

### 1. Resolve or Clone into `~/.agents/<plugin-name>`

1. Determine `<plugin-name>` and source:
   - If the user passed a local path (e.g. `~/.agents/ponytail`), verify it exists.
   - If the user passed a Git URL or `owner/repo`, set target to `"$HOME/.agents/<plugin-name>"`:
     - If missing, clone it: `git clone <url> "$HOME/.agents/<plugin-name>"`.
     - If already present, keep it (only `git pull` if the user asked to update).
2. Never run global installers (`agy plugin install`, `claude plugin install`, `codex plugin add`).

### 2. Analyze the Plugin Structure (or Reuse `references/`)

First check if a pre-analyzed profile already exists in
`references/<plugin-name>.md` (for example [ponytail](references/ponytail.md) or
[humanizer](references/humanizer.md)). If present, reuse that verified blueprint;
otherwise inspect `"$HOME/.agents/<plugin-name>"` before materializing:

- **Skills**:
  - Standard multi-skill layout: `skills/<skill-name>/SKILL.md` (ignore non-skill metadata files like `skills/plugin.json`).
  - Skill-at-root layout: `SKILL.md` directly at repo root (often paired with `agents/openai.yaml`).
  - **Whitelist skill assets**: copy only `SKILL.md` plus assets actually referenced by `SKILL.md` or required by a provider (`references/`, `scripts/` referenced by the skill, `assets/`, `agents/openai.yaml`). Do **not** copy repo-level clutter (`AGENTS.md`, `README.md`, `CHANGELOG.md`, `LICENSE`, `package.json`, `.git/`, `.github/`, `.claude-plugin/`, `.cursor-plugin/`, `tests/`, `benchmarks/`).
- **Hooks & Scripts**:
  - Antigravity native: `hooks.json` at plugin root (top-level keys are hook names, camelCase payloads).
  - Claude / Codex: `hooks/hooks.json`, `hooks/claude-codex-hooks.json`, or `hooks` field in `.claude-plugin/plugin.json` / `.codex-plugin/plugin.json`.
  - Inspect hook scripts for relative `require()` / `../skills/` lookups and copy only the runtime hook scripts & helper modules needed into `.agents/vendor/<plugin-name>/hooks/`.
- **Rules**: `rules/`, `.agents/rules/*.md`, `AGENTS.md`, `CLAUDE.md` — inspect and skip if conflicting with the project's own `AGENTS.md` / `CLAUDE.md`.
- **MCP**: `mcp_config.json`, `.mcp.json`.

### 3. Materialize into `.agents/vendor/<plugin-name>/` & Wire Providers

#### A. Materialize Pruned Runtime (`.agents/vendor/<plugin-name>/`)

```bash
mkdir -p ".agents/vendor/<plugin-name>/skills" .agents/skills .claude/skills

# Multi-skill plugin: copy only skill directories containing SKILL.md
for d in "$HOME/.agents/<plugin-name>"/skills/*/; do
  [ -f "${d}SKILL.md" ] && rsync -a --delete --exclude='.git' "$d" ".agents/vendor/<plugin-name>/skills/$(basename "$d")/"
done

# Or skill-at-root plugin: copy ONLY whitelisted skill files (e.g. SKILL.md, agents/openai.yaml, referenced assets)
mkdir -p ".agents/vendor/<plugin-name>/skills/<plugin-name>"
cp -p "$HOME/.agents/<plugin-name>/SKILL.md" ".agents/vendor/<plugin-name>/skills/<plugin-name>/SKILL.md"

# If plugin ships runtime hook scripts: copy ONLY required runtime scripts into hooks/
mkdir -p ".agents/vendor/<plugin-name>/hooks"
# (copy required runtime hook scripts & helper modules here)
```

#### B. Expose Skills via Intra-Workspace Symlinks (Antigravity + Codex + Claude Code)

```bash
for d in ".agents/vendor/<plugin-name>"/skills/*/; do
  [ -f "${d}SKILL.md" ] || continue
  name="$(basename "$d")"
  ln -sfn "../vendor/<plugin-name>/skills/$name" ".agents/skills/$name"
  ln -sfn "../../.agents/vendor/<plugin-name>/skills/$name" ".claude/skills/$name"
done
```
*(Also symlink this skill into `.claude/skills/` if Claude Code should invoke it:
`ln -sfn "../../.agents/skills/project-plugin-link" ".claude/skills/project-plugin-link"`.)*

#### C. Wire Hooks per Provider (All Pointing Inside `.agents/vendor/<plugin-name>/hooks/`)

- **Antigravity (`.agents/hooks.json`)**: Only merge into `.agents/hooks.json` if the plugin ships **Antigravity-schema** hooks (top-level named hook keys, camelCase payloads), pointing commands at `./vendor/<plugin-name>/hooks/...`. Do **not** feed Claude/Codex `hooks.json` to Antigravity.
- **Codex (`.codex/hooks.json`)**:
  - Read existing `.codex/hooks.json` (create if absent) and **merge** entries under `"hooks"`.
  - Prefix commands with `CLAUDE_PLUGIN_ROOT="$PWD/.agents/vendor/<plugin-name>" PLUGIN_DATA="$PWD/.codex/plugin-data/<plugin-name>"` and execute scripts from `"$PWD/.agents/vendor/<plugin-name>/hooks/..."`.
- **Claude Code (`.claude/settings.local.json`)**:
  - Read existing `.claude/settings.local.json` (create if absent) and **merge** entries under `"hooks"` (do **not** create `.claude/plugins/` symlinks).
  - Prefix commands with `CLAUDE_PLUGIN_ROOT="$CLAUDE_PROJECT_DIR/.agents/vendor/<plugin-name>"` and execute scripts from `"$CLAUDE_PROJECT_DIR/.agents/vendor/<plugin-name>/hooks/..."`.

### 4. Rules & Conflict Guardrails

- **Never overwrite** an existing project `AGENTS.md`, `GEMINI.md`, or `CLAUDE.md`.
- Do **not** copy or activate always-on plugin rule files (`AGENTS.md`, `.agents/rules/*.md`, `<plugin>/rules/`) when the project already has `AGENTS.md`/`CLAUDE.md`, unless explicitly requested. Report skipped rules clearly.
- When merging `.claude/settings.local.json`, `.codex/hooks.json`, or `.agents/hooks.json`, preserve all existing keys and hooks.
- **Exclude locally in Git**: Append `.agents/`, `.claude/skills/`, `.claude/settings.local.json`, and `.codex/` to `.git/info/exclude` (avoiding dirtying tracked `.gitignore`), unless the user asks to commit them.

### 5. Verify & Report

1. Confirm `.agents/vendor/<plugin-name>/` contains only pruned `skills/` and `hooks/` (no repo clutter) and all symlinks in `.agents/skills/` and `.claude/skills/` resolve inside the workspace (`test -f`).
2. If `agy` is installed, verify skill discovery (or note reload needed via `/skills reload`).
3. If `codex` is installed, verify via `codex debug prompt-input` (optional) and dry-run each Codex hook command with `$PWD/.agents/vendor/<plugin-name>/hooks/...`.
4. If `claude` is installed, run `claude plugin validate ".claude/skills"` and dry-run each Claude hook command with `$PWD/.agents/vendor/<plugin-name>/hooks/...`.
5. Summarize:
   - What was materialized into `.agents/vendor/<plugin-name>/` and wired per provider (Antigravity, Codex, Claude Code).
   - What was pruned/skipped (repo clutter, conflicting rules, unsupported hook schemas).
   - How to cleanly remove the plugin from this project (`rm -rf .agents/vendor/<plugin-name>` + its symlinks in `.agents/skills/` and `.claude/skills/` + hook entries + `.codex/plugin-data/<plugin-name>`, leaving `~/.agents/<plugin-name>` intact).
