Use this skill when the user wants commands demonstrated in a persistent shell window.

Workflow:
1. Make sure a project exists (call `add_project` if needed) — every `spawn_agent` call requires an existing `project_id`
2. Create a demo session with `spawn_agent` and `command: "bash"` (supply the `project_id` from step 1)
3. Narrate each step by sending an `echo ...` line with `send_input`
4. Send the real command with `send_input`
5. Leave the demo session running while the user is watching

Demo windows are tracked tasks just like everything else now. When the user is done with the demo, call `complete_task` to tear down the session cleanly (and `complete_project` if the project has no other tasks).
