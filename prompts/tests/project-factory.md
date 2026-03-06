# Project Factory Prompt

This prompt enables continuous generation of interesting projects using multiple parallel agents.

## Usage

Give this prompt to the manager agent, then ask it to start generating projects.

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

1. The manager agent suggests a project broken into 3-5 parallel sub-tasks
2. Each sub-task is assigned to a worker agent via the OMAR HTTP API
3. Workers create their files independently in the `junk/<project-name>/` folder
4. Manager monitors progress and approves pending permissions
5. When complete, manager immediately spawns the next project

## Example Projects Generated

- **url-shortener**: REST API, SQLite storage, analytics, CLI
- **md-preview**: Markdown parser, HTTP/WebSocket server, file watcher, CSS styles
- **task-queue**: Job persistence, worker pool, queue manager, CLI
- **git-stats**: Git data collector, statistics analyzer, HTML report generator, CLI
- **json-validator**: Schema validation, error formatting, file loader, CLI

## Project Structure Pattern

Each project follows a consistent structure:
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

- Workers are spawned with specific, detailed task descriptions
- Workers wait for dependencies (e.g., CLI waits for core modules)
- Manager periodically checks agent status and approves pending actions
- Send "exit" to skip optional follow-up tasks (like running tests)
