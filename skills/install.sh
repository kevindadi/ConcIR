#!/usr/bin/env bash
# Install ConcIR agent skills into coding-agent skill directories.
# Source of truth is this folder (skills/*/SKILL.md). Client-specific
# hidden trees (.cursor, .claude, …) are created only at install time.
set -euo pipefail

usage() {
  cat <<'EOF'
Install ConcIR skills (Agent Skills / SKILL.md) into a coding agent.

Usage:
  ./skills/install.sh [options]

Options:
  --scope project|user   Where to install (default: project)
  --dir PATH             Project root for --scope project (default: cwd)
  --target LIST          Comma-separated clients, or "all" (default: all)
  --skill NAME           Install one skill (default: every skills/*/SKILL.md)
  --copy                 Copy files (default; most clients discover copies)
  --symlink              Symlink back to this repo (live edits)
  --force                Replace an existing install
  --uninstall            Remove installed skills instead of installing
  --dry-run              Print actions only
  -h, --help             Show this help

Targets:
  cursor     project: <dir>/.cursor/skills
             user:    ~/.cursor/skills
  claude     project: <dir>/.claude/skills
             user:    ~/.claude/skills
  codex      project: <dir>/.agents/skills  and  <dir>/.codex/skills
             user:    ~/.agents/skills  and  ${CODEX_HOME:-~/.codex}/skills
  opencode   project: <dir>/.opencode/skills
             user:    ~/.config/opencode/skills
  copilot    project: <dir>/.github/skills
             user:    (not installed; Copilot reads the repo)
  agents     project: <dir>/.agents/skills
             user:    ~/.agents/skills

  all        cursor,claude,codex,opencode

Examples:
  ./skills/install.sh
  ./skills/install.sh --scope user --target cursor
  ./skills/install.sh --target claude,opencode --symlink
  ./skills/install.sh --uninstall --target all
EOF
}

SCOPE="project"
PROJECT_DIR=""
TARGET_SPEC="all"
SKILL_FILTER=""
METHOD="copy"
FORCE=0
UNINSTALL=0
DRY_RUN=0

die() {
  printf 'install.sh: %s\n' "$1" >&2
  exit 1
}

abs_dir() {
  (cd "$1" && pwd)
}

is_skill_dir() {
  [ -f "$1/SKILL.md" ]
}

list_source_skills() {
  for candidate in "$SOURCE_ROOT"/*/ ; do
    [ -d "$candidate" ] || continue
    is_skill_dir "${candidate%/}" || continue
    basename "${candidate%/}"
  done
}

resolve_targets() {
  spec=$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]' | tr ' ' ',')
  if [ "$spec" = "all" ]; then
    printf '%s\n' cursor claude codex opencode
    return
  fi
  old_ifs=$IFS
  IFS=,
  # shellcheck disable=SC2086
  set -- $spec
  IFS=$old_ifs
  for raw in "$@"; do
    t=$(printf '%s' "$raw" | tr -d '[:space:]')
    [ -n "$t" ] || continue
    case "$t" in
      cursor) printf '%s\n' cursor ;;
      claude|claude-code|anthropic) printf '%s\n' claude ;;
      codex|openai) printf '%s\n' codex ;;
      opencode) printf '%s\n' opencode ;;
      copilot|github|github-copilot) printf '%s\n' copilot ;;
      agents|agent) printf '%s\n' agents ;;
      *) die "unknown target '$t' (expected cursor, claude, codex, opencode, copilot, agents, all)" ;;
    esac
  done
}

# Prints destination skill-root directories for one target, one per line.
dest_roots_for() {
  target=$1
  if [ "$SCOPE" = "user" ]; then
    case "$target" in
      cursor) printf '%s\n' "$HOME/.cursor/skills" ;;
      claude) printf '%s\n' "$HOME/.claude/skills" ;;
      codex)
        printf '%s\n' "$HOME/.agents/skills"
        printf '%s\n' "${CODEX_HOME:-$HOME/.codex}/skills"
        ;;
      opencode) printf '%s\n' "$HOME/.config/opencode/skills" ;;
      copilot)
        return 1
        ;;
      agents) printf '%s\n' "$HOME/.agents/skills" ;;
    esac
  else
    case "$target" in
      cursor) printf '%s\n' "$PROJECT_DIR/.cursor/skills" ;;
      claude) printf '%s\n' "$PROJECT_DIR/.claude/skills" ;;
      codex)
        printf '%s\n' "$PROJECT_DIR/.agents/skills"
        printf '%s\n' "$PROJECT_DIR/.codex/skills"
        ;;
      opencode) printf '%s\n' "$PROJECT_DIR/.opencode/skills" ;;
      copilot) printf '%s\n' "$PROJECT_DIR/.github/skills" ;;
      agents) printf '%s\n' "$PROJECT_DIR/.agents/skills" ;;
    esac
  fi
}

run() {
  if [ "$DRY_RUN" -eq 1 ]; then
    printf 'dry-run:'
    for arg in "$@"; do
      printf ' %s' "$arg"
    done
    printf '\n'
    return 0
  fi
  "$@"
}

install_one() {
  src=$1
  dest=$2
  name=$(basename "$src")

  if [ -e "$dest" ] || [ -L "$dest" ]; then
    if [ "$FORCE" -eq 0 ] && [ -L "$dest" ]; then
      current=$(readlink "$dest" || true)
      if [ "$current" = "$src" ]; then
        printf 'skip  %s (already linked)\n' "$dest"
        return 0
      fi
    fi
    if [ "$FORCE" -eq 0 ] && [ ! -L "$dest" ] && [ -d "$dest" ] && [ -f "$dest/SKILL.md" ]; then
      printf 'skip  %s (exists; pass --force to replace)\n' "$dest"
      return 0
    fi
    run rm -rf "$dest"
  fi

  run mkdir -p "$(dirname "$dest")"
  if [ "$METHOD" = "symlink" ]; then
    run ln -s "$src" "$dest"
    printf 'link  %s -> %s\n' "$dest" "$src"
  else
    run cp -R "$src" "$dest"
    printf 'copy  %s\n' "$dest"
  fi
}

uninstall_one() {
  dest=$1
  if [ ! -e "$dest" ] && [ ! -L "$dest" ]; then
    printf 'skip  %s (not installed)\n' "$dest"
    return 0
  fi
  run rm -rf "$dest"
  printf 'rm    %s\n' "$dest"
}

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
SOURCE_ROOT=$SCRIPT_DIR

while [ $# -gt 0 ]; do
  case "$1" in
    --scope)
      [ $# -ge 2 ] || die "--scope needs project|user"
      SCOPE=$2
      shift 2
      ;;
    --dir)
      [ $# -ge 2 ] || die "--dir needs a path"
      PROJECT_DIR=$2
      shift 2
      ;;
    --target)
      [ $# -ge 2 ] || die "--target needs a list"
      TARGET_SPEC=$2
      shift 2
      ;;
    --skill)
      [ $# -ge 2 ] || die "--skill needs a name"
      SKILL_FILTER=$2
      shift 2
      ;;
    --copy) METHOD="copy"; shift ;;
    --symlink|--link) METHOD="symlink"; shift ;;
    --force) FORCE=1; shift ;;
    --uninstall) UNINSTALL=1; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown option '$1' (try --help)" ;;
  esac
done

case "$SCOPE" in
  project|user) ;;
  *) die "--scope must be project or user" ;;
esac

if [ "$SCOPE" = "project" ]; then
  if [ -z "$PROJECT_DIR" ]; then
    PROJECT_DIR=$(pwd)
  fi
  if [ -d "$PROJECT_DIR" ]; then
    PROJECT_DIR=$(abs_dir "$PROJECT_DIR")
  elif [ "$DRY_RUN" -eq 0 ]; then
    die "project directory not found: $PROJECT_DIR"
  fi
fi

SKILLS=""
if [ -n "$SKILL_FILTER" ]; then
  is_skill_dir "$SOURCE_ROOT/$SKILL_FILTER" || die "skill not found: $SKILL_FILTER"
  SKILLS=$SKILL_FILTER
else
  SKILLS=$(list_source_skills)
  [ -n "$SKILLS" ] || die "no skills/*/SKILL.md under $SOURCE_ROOT"
fi

TARGETS=$(resolve_targets "$TARGET_SPEC")
[ -n "$TARGETS" ] || die "no targets selected"

printf 'source  %s\n' "$SOURCE_ROOT"
printf 'scope   %s\n' "$SCOPE"
if [ "$SCOPE" = "project" ]; then
  printf 'project %s\n' "$PROJECT_DIR"
fi
if [ "$UNINSTALL" -eq 1 ]; then
  printf 'action  uninstall\n'
else
  printf 'method  %s\n' "$METHOD"
fi

for skill in $SKILLS; do
  src="$SOURCE_ROOT/$skill"
  for target in $TARGETS; do
    if ! roots=$(dest_roots_for "$target"); then
      printf 'skip  target %s at --scope %s (no destination)\n' "$target" "$SCOPE"
      continue
    fi
    for root in $roots; do
      dest="$root/$skill"
      if [ "$UNINSTALL" -eq 1 ]; then
        uninstall_one "$dest"
      else
        install_one "$src" "$dest"
      fi
    done
  done
done
