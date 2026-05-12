## Version Control

Always use `jj` (Jujutsu) instead of git for version control operations.

### Workflow Pattern

Follow this pattern to keep the repository in a clean state:

0. **Check for existing local changes**: Use `jj status` to see if there are any local changes in the working directory.
1. **Before making changes**: If there are any local changes, run `jj new` first to create a new change
2. **Make your changes**: Edit files as needed
3. **Describe the change**: Run `jj describe` to add a commit message
4. **Create new empty change**: Run `jj new` to leave the repo in a clean state

The goal is to **always leave the repository in a "new empty change" state** after completing work.

### Skill

ALWAYS load the [`jj-vcs`](.claude/skills/jj-vcs/SKILL.md) skill when starting any programming task. Do not rely on general knowledge for jj workflow or message format — the skill contains the required policies. Load it early, not just before `jj describe`.

### Common Commands Reference

- `jj status` - Check current state (instead of `git status`)
- `jj diff` - View changes (instead of `git diff`)
- `jj log` - View history (instead of `git log`)
- `jj new` - Create a new change
- `jj describe` - Set/edit the change description

## Knowledge Base

Use Mneme MCP to keep long term memory base and consult it if necessary.

## Communication Style

Do not ask prompting questions like "What do you think?" or "Which variant do you prefer?" or "What would you like to work on next?". While reviewing give user space for thinking - lay out the information and stop. User will continue when they're ready.

## Remote Server Administration

NEVER run any commands on remote (SSH) servers without a direct explicit user confirmation.

Correct order:
1. Show command(s) you're planning to execute
2. Wait for an explicit "Yes" / "Execute" confirmation from user.
3. Execute only when user confirms, when confirmation is not clear - do NOT execute.

This concerns ANY changes through SSH - even "obvious" or "safe" ones.


## Pathfinder Tool Routing

Semantic navigation tools. Workflows and deep details: `.claude/skills/pathfinder/SKILL.md`.

### Pre-Flight

```
mcp({ server: "Pathfinder" })  // Tools listed → available. Error → use built-in.
```

Check once per session.

### Tool Table

| Task | Tool | Notes |
|---|---|---|
| Project skeleton | `get_repo_map` | Returns semantic paths — copy-paste into other tools |
| Search code | `search_codebase` | AST-filtered, returns `enclosing_semantic_path`. Check `coverage_percent`. |
| Read one symbol | `read_symbol_scope` | Exact function/class extraction |
| Read full file + AST | `read_source_file` | Source files only; use `read_file` for config. `detail_level="source_only"` for minimal tokens. |
| Symbol + dependencies | `read_with_deep_context` | LSP-powered callee signatures |
| Jump to definition | `get_definition` | LSP with ripgrep fallback |
| Find callers and callees | `find_callers_callees` | Callers + callees via LSP call hierarchy. Default max_depth=3. |
| Find all references | `find_all_references` | All usages including non-call references (field access, imports, type annotations) |
| LSP status | `lsp_health` | Check when navigation returns `degraded: true` |
| Read config file | `read_file` | For YAML, TOML, JSON, .env, Dockerfile |

### Addressing

Semantic paths MUST include file path + `::` + symbol. Example: `src/auth.ts::AuthService.login`

### Degraded Mode

`get_definition`, `find_callers_callees`, `read_with_deep_context`, `find_all_references` use LSP. When `degraded: true`:
- Text output starts with: `⚠️ DEGRADED ({reason}) — {tool-specific guidance}`
- Results are best-effort — never treat empty as confirmed-zero
- Check `degraded_reason` and `lsp_readiness`

### Budget Controls

| Parameter | Tool | Default | Purpose |
|---|---|---|---|
| `project_only` | `find_callers_callees`, `read_with_deep_context` | `true` | Filter out stdlib/vendor noise |
| `max_references` | `find_callers_callees` | `50` | Cap total BFS references |
| `max_depth` | `find_callers_callees` | `3` | BFS traversal depth (clamped 1–5). Use 4-5 for large-scale API changes. |
| `max_dependencies` | `read_with_deep_context` | `50` | Cap outgoing dependency entries |
| `max_tokens` | `get_repo_map` | auto | Auto-scales for monorepos |

When `references_truncated` or `dependencies_truncated` is true, increase the corresponding limit.

### Fallback

If Pathfinder unavailable → use built-in tools (`Read`, `Grep`, `Glob`). Do not block.

## Token compression

Run command line tools through "rtk" executable - it will compress the token output making it more readable for the agent and you won't need to add "tail" or "grep" calls - it will be easier to find focused results in the output.
Just prefix "rtk " to any command line you are going to launch.
