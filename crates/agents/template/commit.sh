#!/bin/sh
# Commits the agent's folder after each session, so its history is your view of how it learns and
# `git revert` is your veto. Run by Claude Code's Stop hook, outside the agent's permissions; the app
# says which run and step it was in PERSEPHONE_SESSION.
cd "$(dirname "$0")/.." || exit 0
git add -A >/dev/null 2>&1
git diff --cached --quiet && exit 0
if [ -n "$PERSEPHONE_SESSION" ]; then
  title="$PERSEPHONE_SESSION"
else
  latest=$(ls -t memory/journal/*.md 2>/dev/null | head -1)
  title="Session $(basename "${latest:-session}" .md)"
fi
git commit -q -m "$title" >/dev/null 2>&1
exit 0
