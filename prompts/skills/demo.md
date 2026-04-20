Use this skill when the user wants commands demonstrated in a persistent shell window.

Workflow:
1. Create a demo session with `spawn_agent_session` and `command: "bash"`
2. Narrate each step by sending an `echo ...` line with `send_input`
3. Send the real command with `send_input`
4. Leave the demo session running when finished

Do not use `complete_task` for demo windows. They are not tracked work items.
