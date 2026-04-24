# Project Factory Prompt

Continuously generate projects using multiple parallel agents.

## Usage

Give this to the manager agent, then ask it to start generating projects.

---

## Prompt

```
Please suggest an interesting project to do under the junk folder and make sure that this task requires spawning multiple agents
```

After approving a project idea, say:

```
Yes
```

To enable continuous project generation, say:

```
Great I want to build a workflow that keeps turning out interesting projects like these under the junk folder so feel free to proceed with whatever project that you feel interesting but orchestrate workers just like last time and make sure that they work under the junk folder when a project completes immediately spawn a new interesting project
```

---

## How It Works

1. Manager suggests a project broken into 3-5 parallel sub-tasks.
2. Manager registers the project with `add_project` to get a `project_id`.
3. Each sub-task becomes a tracked OMAR task via the `spawn_agent` MCP tool (one agent per task, all sharing the same `project_id`).
4. Workers create their files independently in `junk/<project-name>/`.
5. Manager polls progress with `get_agent_summary`, `get_agent`, `list_agents`, and scheduled `omar_wake_later` check-ins.
6. When all children are finished or intentionally stopped with `kill_agent`, manager calls `complete_project` and spawns the next project.

## Example Projects Generated

- **url-shortener**: REST API, SQLite storage, analytics, CLI
- **md-preview**: Markdown parser, HTTP/WebSocket server, file watcher, CSS styles
- **task-queue**: Job persistence, worker pool, queue manager, CLI
- **git-stats**: Git data collector, statistics analyzer, HTML report generator, CLI
- **json-validator**: Schema validation, error formatting, file loader, CLI

## Project Structure Pattern

```
junk/<project-name>/
├── package.json
└── src/
    ├── <core-module>.js
    ├── <feature-module>.js
    ├── <utility-module>.js
    └── cli.js
```

## Tips

- Give each task a specific, detailed description (paths, interfaces, expected behavior).
- Use `omar_wake_later` for check-ins; never sleep loops.
- Send "exit" to skip optional follow-up tasks (like running tests).
