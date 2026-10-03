# CLAUDE.md

## Working alongside other agents

- Several agents may work in this repository at the same time. Ignore uncommitted
  changes you didn't make: don't revert, reformat, "fix", or commit them.
- Work on a separate branch, never directly on `master`. Create it in its own
  worktree (`git worktree add -b <branch> ../dupe-rs-<branch> master`) so you
  don't switch or disturb the checkout other agents are using.
- When you're done, commit only the changes your own session made. Stage them
  explicitly (by file, or by hunk for files another agent is also editing) —
  never `git add -A` / `git commit -a`.
- Then merge your branch into `master` (rebase onto `master` first if it has
  moved on, so the merge is a fast-forward), delete the branch, and remove
  the worktree.
