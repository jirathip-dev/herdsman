## Update onto {0} (the PR conflicts with or is behind it)

This is a fix round; a fresh review follows at your pushed head.

1. Merge `origin/{0}` into this branch (`git fetch origin && git merge origin/{0}`). No rebase, no force-push.
2. Resolve the conflicts preserving both sides' intent.
3. Run the repo's gates (build, lint, tests) and push.
4. Report the resolved files.