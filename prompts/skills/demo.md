Use this skill when the user wants commands demonstrated in a persistent shell window.

Workflow:
1. Make sure a project exists (call `add_project` if needed) — every `spawn_agent` call requires an existing `project_id`
2. Create a demo session with `spawn_agent`, `command: "bash"`, and a short required `task` (supply the `project_id` from step 1)
3. Narrate each step by sending an `echo ...` line with `send_input`
4. Send the real command with `send_input`
5. Leave the demo session running while the user is watching

Demo windows are tracked under a project. When the user is done with the demo, call `kill_agent` to tear down the session cleanly, then `complete_project` if the project has no other running sessions.
