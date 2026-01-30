# Workers not automatically grouped under their PM

## Problem

When a PM spawns workers via `POST /api/agents`, those workers don't reliably appear under the PM in the Chain of Command tree. They end up under "Unassigned" instead.

## Current approach (insufficient)

1. **Prompt-based**: PM prompt includes `"parent": "<YOUR NAME>"` in curl examples. Relies on the LLM following instructions — fragile.
2. **Auto-infer (single PM)**: If exactly 1 PM session exists in tmux, auto-assign the worker to it. Fails with multiple PMs.

## Why it's hard

The HTTP API is stateless — there's no way to know *which* tmux session issued the `curl` request. When multiple PMs are running, we can't determine which PM spawned a given worker without explicit information.

## Possible fixes

- **Inject caller identity server-side**: Have each PM's tmux session set an env var or use a unique API token so the server can identify the caller.
- **Convention-based matching**: Require worker names to include their PM prefix (e.g. PM `pm-api` spawns `api-auth`, `api-test`). Parse the prefix to infer parent.
- **Make `parent` mandatory for non-PM spawns**: Return 400 if a worker is spawned without `parent`. Forces all callers (PM prompts) to pass it.
