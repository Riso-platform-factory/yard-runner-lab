---
name: project-plugin-link
description: >-
  Clone a plugin repository into ~/.agents/<name> (if not already present),
  analyze its structure (manifests, skills, rules, hooks, MCP), and install it
  at project scope only for Antigravity, Codex, and Claude Code (copying into
  .agents/ for Antigravity + Codex and symlinking .claude/ from that copy).
  Use when the user asks to attach, link, or test a plugin for the current
  project without installing it globally across all projects.
---

# Project-Scoped Plugin Linker

Install a plugin into the **current repository only** from a shared local cache
at `~/.agents/<plugin-name>`, adapting its layout for **Antigravity**,
**Codex**, and **Claude Code** without touching global AI configs (`~/.claude`,
`~/.codex`, `~/.gemini/config`).

Why copy into `.agents/` instead of symlinking to `~/.agents/`:
- **Antigravity (`agy`)** ignores symlinks during discovery and skips
  external-to-workspace paths in `.agents/skills.json`, so it requires real
  directories inside `.agents/`.
- **Codex** discovers `.agents/skills/` and `.codex/` natively (and follows
  intra-project symlinks).
- **Claude Code** follows relative symlinks in `.claude/`, so `.claude/` can
  symlink directly to the project-local `.agents/` copy.

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
`references/<plugin-name>.md` (for example [ponytail](references/ponytail.md)).
If present, reuse that verified blueprint directly instead of re-analyzing from
scratch; otherwise inspect `"$HOME/.agents/<plugin-name>"` before creating any
links (and optionally save the new profile under `references/<plugin-name>.md`):

- **Manifests**:
  - Antigravity / Gemini: `plugin.json`, `.antigravity-plugin/plugin.json`, `.gemini-plugin/plugin.json`, `gemini-extension.json`
  - Claude Code: `.claude-plugin/plugin.json`
  - Codex: `.codex-plugin/plugin.json`
- **Skills**:
  - Standard multi-skill layout: `skills/<skill-name>/SKILL.md` (ignore metadata files like `skills/plugin.json` when symlinking individual skill dirs).
  - Skill-at-root layout: `SKILL.md` directly at `"$HOME/.agents/<plugin-name>/SKILL.md"` (often paired with `agents/openai.yaml` and `.claude-plugin/plugin.json` `"skills": ["./"]`). Symlink the repo root directly as `.agents/skills/<plugin-name>` and `.claude/skills/<plugin-name>`.
- **Hooks**:
  - Antigravity native: `hooks.json` at plugin root (top-level keys are hook names, camelCase payloads).
  - Claude / Codex: `hooks/hooks.json`, `hooks/claude-codex-hooks.json`, or `hooks` field in `.claude-plugin/plugin.json` / `.codex-plugin/plugin.json`.
  - Check hook commands for runtime requirements (`node`, `python3`, `$CLAUDE_PLUGIN_ROOT`, `$PLUGIN_DATA`).
- **Rules**: `rules/`, `.agents/rules/*.md`, `AGENTS.md`, `CLAUDE.md`.
- **MCP**: `mcp_config.json`, `.mcp.json`.

### 3. Copy Skills into `.agents/skills/` & Wire per Provider (Project Scope Only)

Use `"$HOME/.agents/<plugin-name>"` as the cached source, copy **only the
skill folders** (excluding `.git`) into `.agents/skills/` for **Antigravity +
Codex**, and symlink `.claude/skills/` back to those `.agents/skills/` copies.
Avoid copying entire plugin repositories into `.agents/plugins/` (which creates
unused repo clutter in the workspace) unless the plugin ships Antigravity-native
`hooks.json` / `mcp_config.json`.

#### A. Antigravity + Codex (`.agents/skills/` copy + `.codex/`)

- **Skills (zero symlinks in `.agents/skills/`)**:
  Copy each skill folder directly into `.agents/skills/<skill-name>` (excluding `.git`) so **Antigravity (CLI + IDE)** and **Codex** both discover real directories:
  ```bash
  mkdir -p .agents/skills
  # Multi-skill plugin (skills/<name>/SKILL.md):
  for d in "$HOME/.agents/<plugin-name>"/skills/*/; do
    [ -f "${d}SKILL.md" ] && rsync -a --delete --exclude='.git' "$d" ".agents/skills/$(basename "$d")/"
  done
  # Or skill-at-root plugin (SKILL.md at repo root):
  rsync -a --delete --exclude='.git' "$HOME/.agents/<plugin-name>/" ".agents/skills/<plugin-name>/"
  ```
- **Antigravity hooks**: Only copy/merge `.agents/hooks.json` if the plugin ships an **Antigravity-schema** `hooks.json` (do **not** feed Claude/Codex `hooks.json` to Antigravity).
- **Codex hooks** (if the plugin has Claude/Codex lifecycle hooks):
  - Read existing `.codex/hooks.json` (create if absent) and **merge** entries under `"hooks"` rather than overwriting existing project hooks.
  - Point `CLAUDE_PLUGIN_ROOT` and hook script paths at `"$HOME/.agents/<plugin-name>"` (so `.agents/plugins/` does not need to hold a copy of the whole repo) and set `PLUGIN_DATA="$PWD/.codex/plugin-data/<plugin-name>"` when the hook expects `PLUGIN_DATA` to detect Codex.

#### B. Claude Code (`.claude/` symlinked from `.agents/skills/` copy)

- **Skills** (relative symlinks pointing to the `.agents/skills/` copies):
  ```bash
  mkdir -p .claude/skills
  for d in .agents/skills/*/; do
    [ -f "${d}SKILL.md" ] && ln -sfn "../../.agents/skills/$(basename "$d")" ".claude/skills/$(basename "$d")"
  done
  ```
- **Plugin anchor & Hooks** (only if the plugin has Claude lifecycle hooks):
  - Symlink the plugin anchor for hooks:
    ```bash
    mkdir -p .claude/plugins
    ln -sfn "$HOME/.agents/<plugin-name>" ".claude/plugins/<plugin-name>"
    ```
  - Merge the hook definitions into `.claude/settings.local.json` (so they stay local to this machine/project and do not pollute shared repo settings unless the user asks for `.claude/settings.json`).
  - Prefix commands that read `$CLAUDE_PLUGIN_ROOT` with `CLAUDE_PLUGIN_ROOT="$CLAUDE_PROJECT_DIR/.claude/plugins/<plugin-name>"`.

### 4. Rules & Conflict Guardrails

- **Never overwrite** an existing project `AGENTS.md`, `GEMINI.md`, or `CLAUDE.md`.
- Do **not** auto-link or leave active always-on rule files (e.g. `.agents/rules/*.md` or `<plugin>/rules/`) if the project already has `AGENTS.md`/`CLAUDE.md`, unless the user explicitly asks for always-on rules or the plugin has no skills/hooks of its own. Report skipped rules clearly.
- When merging `.claude/settings.local.json` or `.codex/hooks.json`, preserve all existing keys and hooks.
- **Exclude `.agents/` and provider runtime files locally in Git**: When inside a Git repository, append `.agents/`, `.claude/plugins/`, `.claude/skills/`, `.claude/settings.local.json`, and `.codex/` to `.git/info/exclude` (avoiding dirtying tracked `.gitignore`), unless the user asks to commit them.

### 5. Verify & Report

1. Confirm all `.agents/` copies exist and `.claude/` symlinks resolve (`test -e <path>`).
2. If `agy` is installed and `.agents/plugins/<plugin-name>` exists, run `agy plugin validate ".agents/plugins/<plugin-name>"`.
3. If `claude` is installed, run `claude plugin validate ".claude/skills"` and (if present) `claude plugin validate ".claude/plugins/<plugin-name>"`.
4. Dry-run each wired hook command once with minimal JSON on `stdin` (`echo '{"prompt":"ping"}'`) to confirm exit code `0` and expected output format (and clean up any temporary state if needed, or report).
5. Summarize:
   - What was copied/linked/configured per provider (Antigravity, Codex, Claude Code).
   - What was intentionally skipped (e.g. conflicting always-on rules, unsupported hook schema on a provider).
   - How to unlink cleanly from this project (remove the copied dirs, symlinks, and hook entries without deleting `~/.agents/<plugin-name>`).
