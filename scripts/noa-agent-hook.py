#!/usr/bin/env python3
"""Report Claude Code lifecycle events to the owning Noa terminal."""

import json
import os
import sys
import unicodedata


def report(event):
    if not isinstance(event, dict):
        return None
    name = event.get("hook_event_name")
    state = {
        "SessionStart": "clear",
        "SessionEnd": "clear",
        "UserPromptSubmit": "running",
        "PreToolUse": "running",
        "PostToolUse": "running",
        "PermissionRequest": "permission",
        "Stop": "finished",
        "StopFailure": "error",
    }.get(name)
    if name == "Notification":
        state = {
            "permission_prompt": "permission",
            "idle_prompt": "input",
            "elicitation_dialog": "input",
        }.get(event.get("notification_type"))
    if state is None:
        return None
    # Tool names identify the operation without exposing prompts, arguments,
    # file contents, transcripts, or credentials in the sidebar.
    detail = event.get("tool_name", "") if name in {
        "PreToolUse", "PostToolUse", "PermissionRequest"
    } else ""
    if not isinstance(detail, str):
        detail = ""
    detail = "".join(c for c in detail if not unicodedata.category(c).startswith("C"))[:160]
    return f"\033]777;noa-agent;{state};{detail}\033\\".encode("utf-8")


def main():
    try:
        data = sys.stdin.buffer.read(1024 * 1024 + 1)
        if len(data) > 1024 * 1024:
            return
        payload = report(json.loads(data))
        if payload is None:
            return
        # Hook stdout belongs to the agent's hook protocol, not the terminal.
        # A detached hook without a controlling tty simply has nothing to report.
        fd = os.open("/dev/tty", os.O_WRONLY | os.O_NOCTTY)
        try:
            os.write(fd, payload)
        finally:
            os.close(fd)
    except (OSError, ValueError, TypeError):
        return


if __name__ == "__main__":
    main()
