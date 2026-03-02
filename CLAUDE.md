## Development Guideline

1. Never push to the master / main branch, always push to a feature branch.
2. For a major feature requested by the user, you can push to a feature branch and file a PR. That way the PR can trigger CI.
3. When a CI fails, always investigate what happens and iterate until CI is fixed.
4. When  writing documentation, aim for concision to the point where grammar can be sacrificed. We want documentation to be human-readable. We don't want to write essays.
5. When merging a PR, we always squash all commits before merging.
6. Every feature needs to come with a test, and every test needs to be run in the CI.
7. Always format code before pushing to remote.
8. Always create a new branch off `main` for each PR — never pile unrelated changes onto an existing feature branch.
9. When pushing new code to an existing open PR, always update the PR description to reflect the latest changes.
