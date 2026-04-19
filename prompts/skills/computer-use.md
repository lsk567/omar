OMAR agents can control the desktop through OMAR MCP tools.

Use:
- `computer_status`
- `computer_lock_acquire`
- `computer_lock_release`
- `computer_screenshot`
- `computer_mouse`
- `computer_keyboard`
- `computer_screen_size`
- `computer_mouse_position`

Rules:
- Always acquire the lock before taking screenshots or sending mouse/keyboard actions.
- Release the lock when finished.
- Keep actions deliberate and inspect screenshots between non-trivial UI steps.
