from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("install-agent-skill.sh")
SKILL = (Path(__file__).parent.parent / "skills" / "noa-orchestration").resolve()


def run(*args, home):
    return subprocess.run(
        ["sh", str(SCRIPT), *args],
        capture_output=True,
        text=True,
        env={"HOME": str(home), "PATH": "/usr/bin:/bin"},
    )


class InstallAgentSkillTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.home = Path(self.tmp.name)
        self.link = self.home / ".claude" / "skills" / "noa-orchestration"

    def tearDown(self):
        self.tmp.cleanup()

    def test_links_the_skill_into_the_claude_skills_directory(self):
        result = run(home=self.home)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(self.link.is_symlink())
        self.assertEqual(Path(self.link.readlink()), SKILL)
        self.assertTrue((self.link / "SKILL.md").is_file())

    def test_running_it_again_is_a_no_op(self):
        run(home=self.home)
        result = run(home=self.home)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(Path(self.link.readlink()), SKILL)

    def test_a_directory_can_be_named(self):
        skills = self.home / "project" / ".claude" / "skills"
        result = run("--dir", str(skills), home=self.home)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(Path((skills / "noa-orchestration").readlink()), SKILL)

    def test_something_else_in_the_way_is_left_alone(self):
        self.link.mkdir(parents=True)
        (self.link / "SKILL.md").write_text("mine")
        result = run(home=self.home)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.link.is_symlink())
        self.assertEqual((self.link / "SKILL.md").read_text(), "mine")

    def test_uninstall_removes_only_its_own_link(self):
        run(home=self.home)
        result = run("--uninstall", home=self.home)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(self.link.exists() or self.link.is_symlink())

        self.link.parent.mkdir(parents=True, exist_ok=True)
        self.link.symlink_to(self.home)
        result = run("--uninstall", home=self.home)
        self.assertNotEqual(result.returncode, 0)
        self.assertTrue(self.link.is_symlink())


if __name__ == "__main__":
    unittest.main()
