OMAR supports spawning tracked tasks with backend and model overrides.

Use `list_backends` first when backend availability is uncertain.

When creating a tracked task, set:
- `backend` for the runtime family
- `model` for the concrete model override when supported

Prefer the default backend unless the task clearly benefits from a different tool stack.
