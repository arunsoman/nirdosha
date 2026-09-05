# Nirdosha agent skills

Drop-in instructions that teach an AI coding agent to write valid
Nirdosha (`.nir`) code — syntax rules, types, ownership, builtins, and
a worked example, verified against the real compiler. Every file here
carries the same content (`core.md` is the source of truth), wrapped
for whichever tool you use.

| Tool | File | Where it goes |
|---|---|---|
| Claude Code | [`claude-code/SKILL.md`](./claude-code/SKILL.md) | `.claude/skills/nirdosha/SKILL.md` in your project |
| OpenAI Codex CLI, Amp, and other `AGENTS.md`-reading tools | [`AGENTS.md`](./AGENTS.md) | `AGENTS.md` at your project root |
| Cursor | [`cursor/nirdosha.mdc`](./cursor/nirdosha.mdc) | `.cursor/rules/nirdosha.mdc` in your project |
| GitHub Copilot | [`github/copilot-instructions.md`](./github/copilot-instructions.md) | `.github/copilot-instructions.md` in your project |
| Windsurf | [`windsurfrules`](./windsurfrules) | `.windsurfrules` at your project root |
| Cline | [`clinerules`](./clinerules) | `.clinerules` at your project root |
| Any plain chat interface (ChatGPT, Claude.ai, Gemini, ...) | [`paste-anywhere-prompt.md`](./paste-anywhere-prompt.md) | Nothing to install — copy the content into a new chat |

## Quick install (tool-based agents)

```sh
# from the root of a project that uses Nirdosha
curl -o .claude/skills/nirdosha/SKILL.md --create-dirs \
  https://raw.githubusercontent.com/kannamma-labs/nirdosha/main/agent-skills/nirdosha/claude-code/SKILL.md

# or, for an AGENTS.md-reading tool:
curl -o AGENTS.md \
  https://raw.githubusercontent.com/kannamma-labs/nirdosha/main/agent-skills/nirdosha/AGENTS.md

# or Cursor:
curl -o .cursor/rules/nirdosha.mdc --create-dirs \
  https://raw.githubusercontent.com/kannamma-labs/nirdosha/main/agent-skills/nirdosha/cursor/nirdosha.mdc
```

Swap the URL/destination for the other tools using the table above.

## Why this exists

Nirdosha's grammar is deliberately small and LL(1) — designed so an
LLM sampler can be constrained to only emit syntactically valid tokens
(`crates/compiler/nirdosha.gbnf`, `README.md` §9). These files are the
human-readable half of that same bet: give an agent the actual rules
up front instead of letting it guess from general programming
knowledge and fail on Nirdosha's stricter parts (no `::`, no string
concatenation, `str` banned as a function boundary type, no statement
separators).

## Keeping this current

`core.md` is the single source of truth; every other file in this
directory is that same content with platform-specific wrapping around
it. If you find a factual error, fix it in `core.md` first, then
propagate the fix to the rest (they're intentionally *not* symlinks or
build outputs — most of these platforms can't include external files,
so duplication here is unavoidable). `core.md` itself defers to
`docs/LANGUAGE.md`/`docs/GRAMMAR.md` in the main repo as the actual authoritative
reference — if they ever disagree, those two win.
