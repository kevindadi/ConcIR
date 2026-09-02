# ConcIR agent skills

Portable [Agent Skills](https://agentskills.io/specification) for generating
and repairing ConcIR. This directory is the source of truth. The repository
does not ship client-specific hidden trees; an installer copies or links
each skill into the directories Cursor, Claude Code, Codex, OpenCode, and
GitHub Copilot actually scan.

```text
skills/
  install.sh              # install / uninstall
  generate-concir/        # name must match SKILL.md frontmatter
    SKILL.md
```

## Install

From the ConcIR repository root:

```bash
./skills/install.sh
```

That installs every skill in this folder into the current project for
Cursor, Claude Code, Codex, and OpenCode (copy, not symlink).

```bash
# This machine only, Cursor
./skills/install.sh --scope user --target cursor

# Claude Code + OpenCode in another checkout
./skills/install.sh --dir /path/to/app --target claude,opencode

# Live-edit this repo's SKILL.md
./skills/install.sh --symlink --force

./skills/install.sh --help
```

`--target all` is `cursor,claude,codex,opencode`. Codex is installed under
both `.agents/skills` and `.codex/skills` (or the matching home paths)
because those two roots are what current Codex builds scan.

Project-level destinations are gitignored in this repository so an
install does not show up as a commit.

## After install

Restart or reload the agent so it rescan skills. Then ask it to generate
or repair a ConcIR program; `generate-concir` should activate from its
description. The skill expects a ConcIR checkout (`CONCIR_ROOT` or a
parent that contains `doc/syntax/` and `src/ast.rs`) and runs `cir` /
`cargo run` to validate.
