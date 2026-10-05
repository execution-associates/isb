#!/bin/sh
# Print the id of WORKFLOW's push-to-main run for COMMIT once it has passed.
# Tag workflows use it to promote what main already built and checked
# instead of doing it again, and to refuse a tag whose commit main rejected.
#
#   scripts/main-run.sh WORKFLOW COMMIT [--optional]
#
# Exit 0 with the run id on stdout: the newest such run succeeded.
# Exit 1: it failed or was cancelled, or (without --optional) there is none.
# Exit 3: there is none and --optional was given (the caller does the work).
# While the run is queued or in progress this waits, up to 45 minutes.
# Needs gh with GH_TOKEN (actions: read) and GITHUB_REPOSITORY.
set -eu

[ $# -ge 2 ] || { echo "usage: $0 WORKFLOW COMMIT [--optional]" >&2; exit 2; }
workflow=$1
commit=$2
optional=${3:-}
repo=${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is not set}

i=0
while :; do
	# Newest first; a re-run keeps its id and updates status/conclusion.
	# A lookup that fails (the API is down, the workflow is not on main yet)
	# counts as no run under --optional: the caller then does the work itself.
	if ! row=$(gh run list --repo "$repo" --workflow "$workflow" --commit "$commit" \
		--branch main --event push --limit 1 \
		--json databaseId,status,conclusion \
		-q '.[0] | select(.) | "\(.databaseId) \(.status) \(.conclusion)"'); then
		echo "looking up $workflow runs for $commit failed" >&2
		[ "$optional" = --optional ] && exit 3
		exit 1
	fi
	if [ -z "$row" ]; then
		if [ "$optional" = --optional ]; then
			echo "$workflow has no push-to-main run for $commit" >&2
			exit 3
		fi
		echo "$workflow has no push-to-main run for $commit: tag a commit main's CI has passed" >&2
		exit 1
	fi
	id=${row%% *}
	rest=${row#* }
	status=${rest%% *}
	conclusion=${rest#* }
	if [ "$status" = completed ]; then
		if [ "$conclusion" = success ]; then
			echo "$workflow passed on main for $commit (run $id)" >&2
			echo "$id"
			exit 0
		fi
		echo "$workflow run $id for $commit concluded $conclusion: fix main forward, then tag" >&2
		exit 1
	fi
	i=$((i + 1))
	[ "$i" -le 90 ] || { echo "$workflow run $id for $commit is still $status after 45 minutes" >&2; exit 1; }
	echo "waiting for $workflow run $id ($status, $i)" >&2
	sleep 30
done
