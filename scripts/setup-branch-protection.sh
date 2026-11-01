#!/usr/bin/env bash
# Run once, right after creating a repo from this template, to require PRs into main.
# Usage: scripts/setup-branch-protection.sh <owner>/<repo>
set -euo pipefail

repo="${1:?Usage: setup-branch-protection.sh <owner>/<repo>}"

gh api "repos/${repo}/rulesets" -X POST \
  -f name="main-protection" \
  -f target=branch \
  -f enforcement=active \
  -F 'conditions[ref_name][include][]=refs/heads/main' \
  -F 'conditions[ref_name][exclude][]=' \
  -F 'rules[]={"type":"pull_request"}'

echo "Branch ruleset created on ${repo}: main requires a pull request."
