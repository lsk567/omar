<!-- High-level principles from https://github.com/multica-ai/andrej-karpathy-skills/blob/main/CLAUDE.md -->
# CLAUDE.md

Behavioral guidelines to reduce common LLM coding mistakes. Merge with project-specific instructions as needed.

**Tradeoff:** These guidelines bias toward caution over speed. For trivial tasks, use judgment.

## 1. Think Before Coding

**Don't assume. Don't hide confusion. Surface tradeoffs.**

Before implementing:
- State your assumptions explicitly. If uncertain, ask.
- If multiple interpretations exist, present them - don't pick silently.
- If a simpler approach exists, say so. Push back when warranted.
- If something is unclear, stop. Name what's confusing. Ask.

## 2. Simplicity First

**Minimum code that solves the problem. Nothing speculative.**

- No features beyond what was asked.
- No abstractions for single-use code.
- No "flexibility" or "configurability" that wasn't requested.
- No error handling for impossible scenarios.
- If you write 200 lines and it could be 50, rewrite it.

Ask yourself: "Would a senior engineer say this is overcomplicated?" If yes, simplify.

## 3. Surgical Changes

**Touch only what you must. Clean up only your own mess.**

When editing existing code:
- Don't "improve" adjacent code, comments, or formatting.
- Don't refactor things that aren't broken.
- Match existing style, even if you'd do it differently.
- If you notice unrelated dead code, mention it - don't delete it.

When your changes create orphans:
- Remove imports/variables/functions that YOUR changes made unused.
- Don't remove pre-existing dead code unless asked.

The test: Every changed line should trace directly to the user's request.

## 4. Goal-Driven Execution

**Define success criteria. Loop until verified.**

Transform tasks into verifiable goals:
- "Add validation" → "Write tests for invalid inputs, then make them pass"
- "Fix the bug" → "Write a test that reproduces it, then make it pass"
- "Refactor X" → "Ensure tests pass before and after"

For multi-step tasks, state a brief plan:
```
1. [Step] → verify: [check]
2. [Step] → verify: [check]
3. [Step] → verify: [check]
```

Strong success criteria let you loop independently. Weak criteria ("make it work") require constant clarification.

---

**These guidelines are working if:** fewer unnecessary changes in diffs, fewer rewrites due to overcomplication, and clarifying questions come before implementation rather than after mistakes.

## OMAR-Specific Development Guidelines

1. Never push to the master / main branch, always push to a feature branch.
2. For a major feature requested by the user, you can push to a feature branch and file a PR. That way the PR can trigger CI.
3. When a CI fails, always investigate what happens and iterate until CI is fixed.
4. When  writing documentation, aim for concision to the point where grammar can be sacrificed. We want documentation to be human-readable. We don't want to write essays.
5. When merging a PR, we always squash all commits before merging.
6. Every feature needs to come with a test, and every test needs to be run in the CI.
7. Always format code before pushing to remote.
8. Always create a new branch off `main` for each PR — never pile unrelated changes onto an existing feature branch.
9. When pushing new code to an existing open PR, always update the PR description to reflect the latest changes.
10. When CI tests pass, request reviews from GitHub Copilot and address them judiciously.
