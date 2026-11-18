import contextlib
import importlib.util
import io
import json
from pathlib import Path
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location("noa_agent_hook", Path(__file__).with_name("noa-agent-hook.py"))
hook = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hook)


class AgentHookTests(unittest.TestCase):
    def test_lifecycle_does_not_claim_task_completion(self):
        self.assertEqual(hook.report({"hook_event_name": "Stop"}), b"\x1b]777;noa-agent;finished;\x1b\\")
        self.assertEqual(hook.report({"hook_event_name": "SessionEnd"}), b"\x1b]777;noa-agent;clear;\x1b\\")

    def test_permission_is_explicit_and_does_not_leak_arguments(self):
        payload = hook.report({"hook_event_name": "PermissionRequest", "tool_name": "Bash", "tool_input": {"command": "private contents"}})
        self.assertEqual(payload, b"\x1b]777;noa-agent;permission;Bash\x1b\\")

    def test_unrelated_notifications_are_not_interpreted_as_waiting(self):
        self.assertIsNone(hook.report({"hook_event_name": "Notification", "notification_type": "other"}))
        self.assertIsNone(hook.report([]))

    def test_tool_names_cannot_inject_terminal_commands(self):
        payload = hook.report({"hook_event_name": "PreToolUse", "tool_name": "X\x1b\x07\nY"})
        self.assertEqual(payload, b"\x1b]777;noa-agent;running;XY\x1b\\")

    def test_url_elicitation_waits_for_input(self):
        payload = hook.report({"hook_event_name": "Notification", "notification_type": "elicitation_url_dialog"})
        self.assertEqual(payload, b"\x1b]777;noa-agent;input;\x1b\\")

    def test_without_a_terminal_the_sequence_goes_back_to_claude_code(self):
        out = io.StringIO()
        with mock.patch.object(hook.os, "open", side_effect=OSError(6, "Device not configured")), contextlib.redirect_stdout(out):
            hook.deliver(b"\x1b]777;noa-agent;finished;\x1b\\")
        self.assertEqual(json.loads(out.getvalue()), {"terminalSequence": "\x1b]777;noa-agent;finished;\x1b\\"})


if __name__ == "__main__":
    unittest.main()
