## Demo Window (Running Commands for the User)

When you report steps the user should run (e.g., "here are the steps to run the server"),
and the user asks you to show them, run it, or demonstrate it, you should spawn a plain
bash window and execute the commands there one by one.

The demo window appears in the dashboard as a regular window alongside workers.
The user can select it and press Enter to pop it up. The difference from worker agents:
- Worker agents: you may kill these when the task is done.
- Demo windows: NEVER kill these. The user may want to keep working in them.

### How it works

1. Spawn a bash window (NOT a Claude agent):
```bash
curl -X POST http://localhost:9876/api/agents -H "Content-Type: application/json" -d '{"name": "demo", "command": "bash"}'
```

2. Narrate what you are about to do by sending an echo before each command:
```bash
curl -X POST http://localhost:9876/api/agents/demo/send -H "Content-Type: application/json" -d '{"text": "echo \"--- Step 1: Installing dependencies ---\"", "enter": true}'
```

3. Then send the actual command:
```bash
curl -X POST http://localhost:9876/api/agents/demo/send -H "Content-Type: application/json" -d '{"text": "npm install", "enter": true}'
```

4. Monitor output until the command finishes:
```bash
curl http://localhost:9876/api/agents/demo
```

5. When the output shows the command has completed, narrate and send the next command.

6. When all commands are done, send a final echo:
```bash
curl -X POST http://localhost:9876/api/agents/demo/send -H "Content-Type: application/json" -d '{"text": "echo \"--- Done. This window is yours to use. ---\"", "enter": true}'
```

7. Do NOT kill the demo window. Leave it open for the user.
