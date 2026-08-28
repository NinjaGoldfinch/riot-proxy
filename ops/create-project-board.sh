#!/usr/bin/env bash
# Creates the "riot-proxy" Projects v2 board and adds every open issue to it.
#
# Requires the `project` OAuth scope, which must be granted interactively:
#     gh auth refresh -s project
#
# Safe to re-run: it reuses an existing board of the same name, and adding an
# issue that is already on the board is a no-op.
set -euo pipefail

OWNER="${OWNER:-NinjaGoldfinch}"
REPO="${REPO:-riot-proxy}"
TITLE="${TITLE:-riot-proxy}"

if ! gh auth status 2>&1 | grep -q "project"; then
  echo "Missing the 'project' scope. Run:  gh auth refresh -s project" >&2
  exit 1
fi

number="$(gh project list --owner "$OWNER" --format json \
  | jq -r --arg t "$TITLE" '.projects[] | select(.title == $t) | .number' | head -1)"

if [ -z "$number" ]; then
  echo "Creating project '$TITLE'…"
  number="$(gh project create --owner "$OWNER" --title "$TITLE" --format json | jq -r '.number')"
else
  echo "Reusing existing project #$number"
fi

project_url="$(gh project view "$number" --owner "$OWNER" --format json | jq -r '.url')"

echo "Adding issues from $OWNER/$REPO…"
gh issue list --repo "$OWNER/$REPO" --state all --limit 200 --json url \
  | jq -r '.[].url' \
  | while read -r issue_url; do
      gh project item-add "$number" --owner "$OWNER" --url "$issue_url" >/dev/null
      echo "  + $issue_url"
    done

echo
echo "Board ready: $project_url"
echo
echo "Suggested next step — set Status on each item in the board UI:"
echo "  Done        #1–#8   (phases 0–7, shipped and verified)"
echo "  In progress #9      (Phase 8: infra shipped, deploy outstanding)"
echo "  Todo        #10–#13 (blocked on a Riot key, plus follow-ups)"
