from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("install-agent-skill.sh")
SKILLS = (Path(__file__).parent.parent / "skills").resolve()
NAMES = sorted(p.parent.name for p in SKILLS.glob("*/SKILL.md"))
AGENT_DIRS = {
    "claude": Path(".claude/skills"),
    "codex": Path(".codex/skills"),
    "agy": Path(".gemini/antigravity-cli/skills"),
}


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

    def tearDown(self):
        self.tmp.cleanup()

    def skills_dir(self, agent):
        return self.home / AGENT_DIRS[agent]

    def install_hosts(self, *agents):
        for agent in agents:
            self.skills_dir(agent).parent.mkdir(parents=True, exist_ok=True)

    def assertLinked(self, skills):
        for name in NAMES:
            link = skills / name
            self.assertTrue(link.is_symlink(), link)
            self.assertEqual(Path(link.readlink()), SKILLS / name)
            self.assertTrue((link / "SKILL.md").is_file())

    def test_the_repo_has_more_than_one_skill(self):
        self.assertIn("noa-orchestration", NAMES)
        self.assertIn("noa-factory", NAMES)

    def test_links_every_skill_into_every_installed_agent(self):
        self.install_hosts("claude", "codex", "agy")
        result = run(home=self.home)
        self.assertEqual(result.returncode, 0, result.stderr)
        for agent in AGENT_DIRS:
            self.assertLinked(self.skills_dir(agent))

    def test_an_agent_that_is_not_installed_is_skipped(self):
        self.install_hosts("claude")
        result = run(home=self.home)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertLinked(self.skills_dir("claude"))
        self.assertFalse((self.home / ".codex").exists())
        self.assertFalse((self.home / ".gemini").exists())
        self.assertIn("skip codex", result.stdout)

    def test_running_it_again_is_a_no_op(self):
        self.install_hosts("claude", "codex", "agy")
        run(home=self.home)
        result = run(home=self.home)
        self.assertEqual(result.returncode, 0, result.stderr)
        for agent in AGENT_DIRS:
            self.assertLinked(self.skills_dir(agent))

    def test_a_named_agent_is_installed_even_without_its_home(self):
        result = run("--agent", "agy", home=self.home)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertLinked(self.skills_dir("agy"))
        self.assertFalse((self.home / ".claude").exists())

    def test_an_unknown_agent_is_refused(self):
        result = run("--agent", "vim", home=self.home)
        self.assertEqual(result.returncode, 2)

    def test_a_directory_can_be_named(self):
        skills = self.home / "project" / ".claude" / "skills"
        self.install_hosts("claude")
        result = run("--dir", str(skills), home=self.home)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertLinked(skills)
        self.assertFalse(self.skills_dir("claude").exists())

    def test_something_else_in_the_way_is_left_alone(self):
        self.install_hosts("claude", "codex")
        mine = self.skills_dir("claude") / "noa-orchestration"
        mine.mkdir(parents=True)
        (mine / "SKILL.md").write_text("mine")
        result = run(home=self.home)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(mine.is_symlink())
        self.assertEqual((mine / "SKILL.md").read_text(), "mine")
        # The rest is still installed.
        self.assertTrue((self.skills_dir("claude") / "noa-factory").is_symlink())
        self.assertLinked(self.skills_dir("codex"))

    def test_uninstall_removes_only_its_own_links(self):
        self.install_hosts("claude", "codex", "agy")
        run(home=self.home)
        result = run("--uninstall", home=self.home)
        self.assertEqual(result.returncode, 0, result.stderr)
        for agent in AGENT_DIRS:
            for name in NAMES:
                link = self.skills_dir(agent) / name
                self.assertFalse(link.exists() or link.is_symlink())

        other = self.skills_dir("claude") / "noa-orchestration"
        other.symlink_to(self.home)
        result = run("--uninstall", home=self.home)
        self.assertNotEqual(result.returncode, 0)
        self.assertTrue(other.is_symlink())


if __name__ == "__main__":
    unittest.main()
