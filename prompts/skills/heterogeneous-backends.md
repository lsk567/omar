## Heterogeneous Backends

OMAR supports spawning agents with different backends and models. Use this when tasks benefit from specific tools (e.g., codex for OpenAI models, opencode for multi-provider support).

### Check available backends
```bash
curl http://localhost:9876/api/backends
```
Returns which backends are installed, their resolved commands, and availability.

### Spawn with a specific backend
```bash
curl -X POST http://localhost:9876/api/agents \
  -H "Content-Type: application/json" \
  -d '{"name": "worker", "task": "...", "parent": "<YOUR NAME>", "backend": "codex", "model": "o3"}'
```

### Supported backends

| Backend    | Shorthand    | Model flag format                          |
|------------|--------------|--------------------------------------------|
| Claude Code| `"claude"`   | `--model <model-id>`                       |
| Codex CLI  | `"codex"`    | `--model <model-id>`                       |
| Cursor     | `"cursor"`   | `--model` passed through but may be ignored |
| Gemini CLI | `"gemini"`   | `--model <model-id>`                       |
| OpenCode   | `"opencode"` | `--model <provider>/<model-id>`            |

### Common model names

**Claude (Anthropic):**
- `claude-sonnet-4-5-20250514` — fast, good for most tasks
- `claude-opus-4-5-20250514` — strongest reasoning

**Codex (OpenAI):**
- `o3` — strong reasoning
- `o4-mini` — fast and cheap

**OpenCode (multi-provider):**
- `anthropic/claude-sonnet-4-5-20250514`
- `openai/o3`

### When to mix backends

- Default backend handles most tasks well — don't over-optimize
- Use a different backend when the task specifically benefits from it (e.g., OpenAI models for certain code patterns, or a cheaper model for simple tasks)
- Always check `GET /api/backends` first to confirm availability
