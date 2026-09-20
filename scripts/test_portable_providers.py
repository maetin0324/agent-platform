import importlib.util
from pathlib import Path
import unittest


spec = importlib.util.spec_from_file_location("portable", Path(__file__).with_name("portable-providers.py"))
portable = importlib.util.module_from_spec(spec)
spec.loader.exec_module(portable)


class MigrationTests(unittest.TestCase):
    def test_only_general_harness_pins_change_and_second_pass_is_noop(self):
        original = '''# Keep this comment.
[[harnesses]]
id = "conversation"
adapter = "claude-code" # old pin
instructions = "Keep the contract"
[[harnesses]]
id = "literature"
adapter = "paperqa"
[[providers]]
id = "claude"
adapter = "claude-code"
env = { SECRET = "must-stay-private" }
'''
        updated, changes = portable.migrate(original)
        self.assertEqual(len(changes), 1)
        self.assertEqual(updated, original.replace('adapter = "claude-code" # old pin\n', ""))
        self.assertNotIn("must-stay-private", str(changes))
        self.assertEqual(portable.migrate(updated), (updated, []))

    def test_legacy_roles_and_nested_tables(self):
        original = '''[[roles]]
id = "secretary"
adapter = "codex"
[roles.extra]
value = "unchanged"
[[harnesses]]
id = "custom-specialist"
adapter = "claude-code"
'''
        updated, changes = portable.migrate(original)
        self.assertEqual(len(changes), 1)
        self.assertEqual(updated, original.replace('adapter = "codex"\n', ""))

    def test_multiline_instructions_are_not_rewritten(self):
        original = '''[[harnesses]]
id = "coding"
adapter = "claude-code"
instructions = """example:
adapter = "codex"
"""
'''
        # Refuse ambiguous line-oriented edits rather than corrupt the instructions.
        with self.assertRaises(ValueError):
            portable.migrate(original)


if __name__ == "__main__":
    unittest.main()
