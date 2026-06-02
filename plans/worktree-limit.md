# Limit nav Worktrees to 10

## Goal
Keep nav-managed Git worktrees bounded to a maximum of 10 per repository without deleting user work or surprising the user.

## Scope
Only manage worktrees created by nav:

- Directory: `.nav/worktrees/nav-wt-*`
- Branch pattern: `nav-wt/<uuid-v7>`

Do not prune arbitrary Git worktrees outside nav's managed directory.

## Recommended behavior

1. Before creating a new worktree, acquire a repository-local lock.
2. Discover nav-managed worktrees from `git worktree list --porcelain`.
3. If fewer than 10 exist, create the new worktree normally.
4. If 10 or more exist, prune safe candidates until there are at most 9, then create the new one.
5. If not enough safe candidates exist, refuse to create the new worktree and show a clear error.

## Safe-to-prune rules
A nav-managed worktree can be automatically removed only when all are true:

- The path is under `.nav/worktrees/`.
- The name matches `nav-wt-*`.
- No active nav session is currently using it.
- `git status --porcelain` in that worktree is clean.
- The branch can be deleted with normal `git branch -d`, meaning Git does not consider it unmerged.

Never auto-delete a dirty worktree or a branch with unique/unmerged commits.

## Pruning order
Use least-recently-used ordering:

1. `last_used_at` from nav metadata, if available.
2. Creation time derived from the UUID v7 in the worktree/branch name.
3. Filesystem modification time as a fallback.

## Locking
Use a per-repository lock around prune/create operations, for example:

- `.nav/worktrees/.lock`

This prevents two nav instances from concurrently seeing room available and both creating worktrees.

## Metadata
Maintain lightweight metadata, ideally:

- `.nav/worktrees/index.json`

Suggested fields:

- `id`
- `path`
- `branch`
- `session_id`
- `created_at`
- `last_used_at`

Git remains the source of truth, but metadata improves LRU pruning and UI/debuggability.

## CLI/API additions
Recommended commands or API methods:

- `nav worktree list`
- `nav worktree prune`
- `nav worktree remove <id>`
- `nav worktree prune --force` for explicit destructive cleanup only

## Error behavior
When the limit is reached and no safe candidates can be removed, fail clearly:

> nav has reached the maximum of 10 worktrees. None can be safely pruned because they contain uncommitted changes, unmerged commits, or are attached to active sessions. Remove one manually or run an explicit force prune.

## Summary
The preferred policy is automatic safe pruning plus explicit refusal when cleanup could risk losing work.
