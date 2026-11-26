#!/usr/bin/env python3
"""Check that every widget under widgets/ is registered everywhere it must be.

Usage: python3 -I -B scripts/check-widget-registration.py [--repo=DIR] [--json]

The rules are the ones in docs/specs/scaffold-widget.md (I1 registry, I2 output,
I3 derivation rules). This script only reads files: it starts no process and
runs no code from the repository it scans. widgets/scaffold carries a Rust port
of the same rules; both are tested against the fixtures in
scripts/tests/fixtures/widget-registration/.

Exit status: 2 if a source could not be read or parsed, else 1 if a registration
disagrees with dev/scaffold/registry.json, else 0.
"""

import json
import os
import re
import stat
import sys

SCOPE = "Checked: registration consistency only. Not checked: the contents of built artifacts."
MAX_BYTES = 1048576
PLATFORMS = ("linux", "macos")

REGISTRY = "dev/scaffold/registry.json"
HELPER = "crates/noa-app/src/helper_program.rs"
APP_COMMANDS = "crates/noa-app/src/app/commands.rs"
COMMAND = "crates/noa-app/src/commands/command.rs"
MENU = "crates/noa-app/src/macos_menu.rs"
PALETTE = "crates/noa-app/src/command_palette.rs"
VERIFY = "scripts/verify.sh"
BUNDLE = "scripts/bundle-macos.sh"
PACKAGE = "scripts/package-linux.sh"

CONST_RE = r"pub\(crate\) const ([A-Z][A-Z0-9_]*): HelperProgram = HelperProgram \{(.*?)\};"
FIELD_RE = r'(\w+): "([^"]*)"'
FOR_WIDGET_RE = r"^for widget in (.+); do$"

POINT_ORDER = (
    "crate", "launch", "command", "menu", "palette", "verify", "macos_bundle", "linux_package",
)
FACT_KEYS = {
    "crate": ("binary", "accepts_repo"),
    "launch": ("const", "bundled", "repo_arg"),
    "command": ("variant", "action"),
    "menu": (),
    "palette": (),
    "verify": ("check",),
    "macos_bundle": ("name",),
    "linux_package": ("name",),
}


class Source:
    """One file the checker reads: its lines, or why it could not be used."""

    def __init__(self, path):
        self.path = path
        self.error = None
        self.lines = []
        self.anchor = 1

    @property
    def ok(self):
        return self.error is None

    @property
    def text(self):
        return "\n".join(self.lines)


def abs_path(repo, rel):
    return os.path.join(repo, *rel.split("/"))


def load(repo, rel):
    """Read `rel` as I3 prescribes; the result is a Source with `error` set on failure."""
    src = Source(rel)
    try:
        st = os.lstat(abs_path(repo, rel))
    except OSError:
        src.error = "missing"
        return src
    if not stat.S_ISREG(st.st_mode):
        src.error = "not a regular file"
        return src
    if st.st_size > MAX_BYTES:
        src.error = "too large"
        return src
    try:
        with open(abs_path(repo, rel), "rb") as handle:
            raw = handle.read(MAX_BYTES + 1)
    except OSError:
        src.error = "missing"
        return src
    if len(raw) > MAX_BYTES:
        src.error = "too large"
        return src
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError:
        src.error = "not UTF-8"
        return src
    lines = text.split("\n")
    if lines and lines[-1] == "":
        lines.pop()
    src.lines = [line[:-1] if line.endswith("\r") else line for line in lines]
    return src


def line_of(text, offset):
    return text.count("\n", 0, offset) + 1


def first_line(src, needle):
    for number, line in enumerate(src.lines, 1):
        if needle in line:
            return number
    return None


def function_body(src, name):
    """(first line number, lines) of `fn NAME`, or None if it cannot be bounded."""
    for index, line in enumerate(src.lines):
        if "fn " + name + "(" in line:
            indent = line[: len(line) - len(line.lstrip())]
            for end in range(index + 1, len(src.lines)):
                if src.lines[end] == indent + "}":
                    return index + 1, src.lines[index : end + 1]
            return None
    return None


def fail_anchor(src, anchor):
    src.error = "anchor not found: " + anchor


def anchor_functions(src, names):
    """Anchor on `fn NAME` for each name, in order; the first sets the anchor line."""
    for position, name in enumerate(names):
        number = first_line(src, "fn " + name)
        if number is None or function_body(src, name) is None:
            fail_anchor(src, "fn " + name)
            return
        if position == 0:
            src.anchor = number


def read_helper(repo):
    src = load(repo, HELPER)
    if not src.ok:
        return src
    anchor_functions(src, ["launch_args"])
    if not src.ok:
        return src
    if re.search(CONST_RE, src.text, re.DOTALL) is None:
        fail_anchor(src, "HelperProgram const")
    return src


def read_commands(repo):
    src = load(repo, APP_COMMANDS)
    if src.ok:
        anchor_functions(src, ["open_helper_program"])
    return src


def read_command(repo):
    src = load(repo, COMMAND)
    if src.ok:
        anchor_functions(src, ["menu_id", "from_menu_id", "fixed_action_name", "from_action_name"])
    return src


def read_menu(repo):
    src = load(repo, MENU)
    if src.ok:
        number = first_line(src, "MenuItem::with_id")
        if number is None:
            fail_anchor(src, "MenuItem::with_id")
        else:
            src.anchor = number
    return src


def read_palette(repo):
    src = load(repo, PALETTE)
    if src.ok:
        anchor_functions(src, ["command_palette_title", "command_palette_entries"])
    return src


def read_verify(repo):
    src = load(repo, VERIFY)
    if src.ok:
        number = first_line(src, "widget_check()")
        if number is None:
            fail_anchor(src, "widget_check()")
        else:
            src.anchor = number
    return src


def for_widget_line(src):
    for number, line in enumerate(src.lines, 1):
        match = re.match(FOR_WIDGET_RE, line)
        if match:
            return number, match.group(1).split(" ")
    return None


def read_bundle(repo):
    src = load(repo, BUNDLE)
    if src.ok:
        found = for_widget_line(src)
        if found is None:
            fail_anchor(src, "for widget in")
        else:
            src.anchor = found[0]
    return src


def package_block(src):
    """(line of `WIDGETS=(`, the lines up to the closing `)`), or None."""
    for index, line in enumerate(src.lines):
        if line == "WIDGETS=(":
            for end in range(index + 1, len(src.lines)):
                if src.lines[end] == ")":
                    return index + 1, list(enumerate(src.lines[index + 1 : end], index + 2))
            return None
    return None


def read_package(repo):
    src = load(repo, PACKAGE)
    if src.ok:
        block = package_block(src)
        if block is None:
            fail_anchor(src, "WIDGETS=(")
        else:
            src.anchor = block[0]
    return src


def read_registry(repo):
    """(Source, parsed registry or None). A source with an error has no registry."""
    src = load(repo, REGISTRY)
    if not src.ok:
        return src, None
    try:
        data = json.loads(src.text)
    except ValueError:
        src.error = "invalid JSON"
        return src, None
    number = first_line(src, '"widgets"')
    src.anchor = number if number is not None else 1
    if not isinstance(data, dict) or type(data.get("schema")) is not int or data["schema"] != 1:
        src.error = "invalid registry: schema is not 1"
        return src, None
    widgets = data.get("widgets")
    if not isinstance(widgets, dict):
        src.error = "invalid registry: widgets is not an object"
        return src, None
    for name in sorted(widgets):
        entry = widgets[name]
        ships = entry.get("ships") if isinstance(entry, dict) else None
        if (
            not isinstance(ships, list)
            or not ships
            or not all(isinstance(p, str) and p in PLATFORMS for p in ships)
        ):
            src.error = "invalid registry: ships of %s is not a non-empty list of linux/macos" % name
            return src, None
        if "reason" in entry and not isinstance(entry["reason"], str):
            src.error = "invalid registry: reason of %s is not a string" % name
            return src, None
    if not isinstance(data.get("assets"), list):
        src.error = "invalid registry: assets is not a list"
        return src, None
    return src, data


def discover_widgets(repo):
    """(Source for `widgets`, sorted widget names)."""
    src = Source("widgets")
    root = abs_path(repo, "widgets")
    try:
        st = os.lstat(root)
    except OSError:
        src.error = "missing"
        return src, []
    if not stat.S_ISDIR(st.st_mode):
        src.error = "not a directory"
        return src, []
    names = []
    try:
        children = os.listdir(root)
    except OSError:
        src.error = "missing"
        return src, []
    for name in children:
        if name.startswith("."):
            continue
        try:
            child = os.lstat(os.path.join(root, name))
        except OSError:
            continue
        if not stat.S_ISDIR(child.st_mode):
            continue
        try:
            os.lstat(os.path.join(root, name, "Cargo.toml"))
        except OSError:
            continue
        names.append(name)
    return src, sorted(names)


def read_cargo(repo, widget):
    """(Source, package name line, package name, binary name)."""
    src = load(repo, "widgets/%s/Cargo.toml" % widget)
    if not src.ok:
        return src, None, None, None
    package = None
    for index, line in enumerate(src.lines):
        if line != "[package]":
            continue
        for number in range(index + 1, len(src.lines)):
            if src.lines[number].startswith("["):
                break
            match = re.match(r'^name = "([^"]+)"$', src.lines[number])
            if match:
                package = (number + 1, match.group(1))
                break
        break
    if package is None:
        fail_anchor(src, "[package] name")
        return src, None, None, None
    src.anchor = package[0]
    binary = package[1]
    for index, line in enumerate(src.lines):
        if line != "[[bin]]":
            continue
        named = None
        for number in range(index + 1, len(src.lines)):
            if src.lines[number].startswith("["):
                break
            match = re.match(r'^name = "([^"]+)"$', src.lines[number])
            if match:
                named = match.group(1)
                break
        if named is not None:
            binary = named
            break
    return src, package[0], package[1], binary


def evidence_sorted(items):
    return [
        {"path": path, "line": line}
        for path, line in sorted({(item[0], item[1]) for item in items})
    ]


def make_point(state, facts, missing=(), evidence=()):
    point = {"state": state, "missing": list(missing), "evidence": evidence_sorted(evidence)}
    point.update(facts)
    return point


def unknown_point(name):
    return make_point("unknown", {key: None for key in FACT_KEYS[name]})


def ordered(parts, found):
    return [part for part in parts if part not in found]


def crate_point(widget, cargo, main, package_line, binary):
    if not cargo.ok or not main.ok:
        return unknown_point("crate")
    evidence = [(cargo.path, package_line)]
    accepts = first_line(main, '"--repo="')
    if accepts is not None:
        evidence.append((main.path, accepts))
    return make_point(
        "present", {"binary": binary, "accepts_repo": accepts is not None}, evidence=evidence
    )


def launch_point(widget, helper):
    """(point, the const's name or None)."""
    if not helper.ok:
        return unknown_point("launch"), None
    text = helper.text
    for match in re.finditer(CONST_RE, text, re.DOTALL):
        fields = {}
        crate_at = None
        for field in re.finditer(FIELD_RE, match.group(2)):
            fields[field.group(1)] = field.group(2)
            if field.group(1) == "crate_name":
                crate_at = match.start(2) + field.start()
        if fields.get("crate_name") != widget:
            continue
        const = match.group(1)
        body = function_body(helper, "launch_args")[1]
        repo_arg = any("helper.crate_name == %s.crate_name" % const in line for line in body)
        evidence = [(helper.path, line_of(text, crate_at))]
        facts = {"const": const, "bundled": fields.get("bundled"), "repo_arg": repo_arg}
        return make_point("present", facts, evidence=evidence), const
    return make_point("absent", {"const": None, "bundled": None, "repo_arg": None}), None


def command_point(const, commands, command):
    if const is None or not commands.ok or not command.ok:
        return unknown_point("command")
    text = commands.text
    pattern = (
        r"AppCommand::([A-Za-z0-9]+)\s*=>\s*\{?\s*self\.open_helper_program\(\s*"
        r"(crate::helper_program::%s)\s*\)" % re.escape(const)
    )
    match = re.search(pattern, text)
    if match is None:
        return make_point("absent", {"variant": None, "action": None})
    variant = match.group(1)
    evidence = [(commands.path, line_of(text, match.start(2)))]
    found = {"dispatch"}

    def body_lines(name):
        start, lines = function_body(command, name)
        return [(start + offset, line) for offset, line in enumerate(lines)]

    action = None
    action_line = None
    for number, line in body_lines("fixed_action_name"):
        hit = re.search(r'Self::%s => "([a-z0-9-]+\.open)"' % re.escape(variant), line)
        if hit:
            action, action_line = hit.group(1), number
            break
    if action is not None:
        found.add("action")
        evidence.append((command.path, action_line))
        for _, line in body_lines("from_action_name"):
            if '"%s"' % action in line and "=> Some(Self::%s)" % variant in line:
                found.add("action-lookup")
                break
    menu_const = None
    for _, line in body_lines("menu_id"):
        hit = re.search(r"AppCommand::%s => Self::([A-Z0-9_]+)" % re.escape(variant), line)
        if hit:
            menu_const = hit.group(1)
            found.add("menu-id")
            break
    if menu_const is not None:
        for _, line in body_lines("from_menu_id"):
            if "Self::%s => Some(Self::%s)" % (menu_const, variant) in line:
                found.add("menu-id-lookup")
                break
    missing = ordered(["action", "action-lookup", "menu-id", "menu-id-lookup"], found)
    state = "partial" if missing else "present"
    return make_point(state, {"variant": variant, "action": action}, missing, evidence)


def menu_point(variant, menu):
    if variant is None or not menu.ok:
        return unknown_point("menu")
    number = first_line(menu, "AppCommand::%s.menu_id()" % variant)
    if number is None:
        return make_point("absent", {})
    return make_point("present", {}, evidence=[(menu.path, number)])


def palette_point(variant, palette):
    if variant is None or not palette.ok:
        return unknown_point("palette")
    evidence = []
    entry_lines = function_body(palette, "command_palette_entries")
    title_lines = function_body(palette, "command_palette_title")
    entry = title = False
    for offset, line in enumerate(entry_lines[1]):
        if re.match(r"^\s*AppCommand::%s,\s*$" % re.escape(variant), line):
            entry = True
            evidence.append((palette.path, entry_lines[0] + offset))
            break
    for offset, line in enumerate(title_lines[1]):
        if re.search(r'AppCommand::%s => "' % re.escape(variant), line):
            title = True
            evidence.append((palette.path, title_lines[0] + offset))
            break
    if not entry:
        return make_point("absent", {}, evidence=evidence)
    if not title:
        return make_point("partial", {}, ["title"], evidence)
    return make_point("present", {}, evidence=evidence)


def verify_point(widget, verify):
    if not verify.ok:
        return unknown_point("verify")
    escaped = re.escape(widget)
    var = None
    evidence = []
    for number, line in enumerate(verify.lines, 1):
        match = re.match(r"^(\w+)=widgets/%s/Cargo\.toml$" % escaped, line)
        if match:
            var = match.group(1)
            evidence.append((verify.path, number))
            break
    check = None
    if var is not None:
        for number, line in enumerate(verify.lines, 1):
            match = re.match(r'^\s*(?:in_part (?:core|widgets) && )?run (\S+) (widget_check|usage_check) "\$%s"\s*$' % re.escape(var), line)
            if match:
                check = match.group(2)
                evidence.append((verify.path, number))
                break
    if var is None or check is None:
        return make_point("absent", {"check": None}, evidence=evidence)
    found = {"manifest", "full-run"}
    for line in verify.lines:
        match = re.match(r"^\s*case \$path in ([^)]*)\)", line)
        if match and "widgets/%s/*" % widget in [alt.strip() for alt in match.group(1).split("|")]:
            found.add("changed-path")
        if re.match(r'^\s*\[ "\$\w+" = 1 \] && run \S+ (widget_check|usage_check) "\$%s"\s*$' % re.escape(var), line):
            found.add("changed-run")
        if 'widget_uses "$%s"' % var in line:
            found.add("widget-uses")
    missing = ordered(["changed-path", "changed-run", "widget-uses"], found)
    state = "partial" if missing else "present"
    return make_point(state, {"check": check}, missing, evidence)


def bundle_point(widget, bundle):
    if not bundle.ok:
        return unknown_point("macos_bundle")
    var = None
    for line in bundle.lines:
        match = re.match(r'^(\w+)="\$TARGET_DIR/%s"$' % re.escape(widget), line)
        if match:
            var = match.group(1)
            break
    name = None
    copy_line = None
    if var is not None:
        for number, line in enumerate(bundle.lines, 1):
            match = re.match(r'^cp "\$%s" "\$CONTENTS/Helpers/([^"]+)"$' % re.escape(var), line)
            if match:
                name, copy_line = match.group(1), number
                break
    if var is None or name is None:
        return make_point("absent", {"name": None})
    found = {"binary", "copy"}
    listed = for_widget_line(bundle)
    if listed is not None and widget in listed[1]:
        found.add("build")
    for line in bundle.lines:
        if '[ -x "$%s" ]' % var in line:
            found.add("check")
        if '--sign "$NOA_SIGN_IDENTITY" "$CONTENTS/Helpers/%s"' % name in line:
            found.add("sign")
    missing = ordered(["build", "check", "sign"], found)
    state = "partial" if missing else "present"
    return make_point(state, {"name": name}, missing, [(bundle.path, copy_line)])


def package_point(widget, package):
    if not package.ok:
        return unknown_point("linux_package")
    for number, line in package_block(package)[1]:
        match = re.match(r'^\s*"([a-z0-9-]+):([^"]+)"\s*$', line)
        if match and match.group(1) == widget:
            return make_point(
                "present", {"name": match.group(2)}, evidence=[(package.path, number)]
            )
    return make_point("absent", {"name": None})


def entry_line(registry, key):
    """The line of a widget's registry entry: the first `"key":` after `"widgets"`."""
    pattern = r'^\s*"%s"\s*:' % re.escape(key)
    for number in range(registry.anchor + 1, len(registry.lines) + 1):
        if re.match(pattern, registry.lines[number - 1]):
            return number
    return registry.anchor


def evidence_of(point):
    return [(item["path"], item["line"]) for item in point["evidence"]]


def build(repo):
    registry_src, registry = read_registry(repo)
    widgets_src, names = discover_widgets(repo)
    helper = read_helper(repo)
    commands = read_commands(repo)
    command = read_command(repo)
    menu = read_menu(repo)
    palette = read_palette(repo)
    verify = read_verify(repo)
    bundle = read_bundle(repo)
    package = read_package(repo)

    sources = [registry_src, widgets_src, helper, commands, command, menu, palette, verify, bundle, package]
    reports = []
    findings = []
    explained = []

    def add(rule, widget, target, evidence, parts=()):
        findings.append(
            {
                "rule": rule,
                "widget": widget,
                "target": target,
                "parts": list(parts),
                "evidence": evidence_sorted(evidence),
            }
        )

    registry_widgets = registry["widgets"] if registry is not None else {}
    for widget in names:
        cargo, package_line, package_name, binary = read_cargo(repo, widget)
        main = load(repo, "widgets/%s/src/main.rs" % widget)
        sources.extend([cargo, main])
        crate = crate_point(widget, cargo, main, package_line, binary)
        launch, const = launch_point(widget, helper)
        command_p = command_point(const if launch["state"] == "present" else None, commands, command)
        variant = command_p.get("variant")
        points = {
            "crate": crate,
            "launch": launch,
            "command": command_p,
            "menu": menu_point(variant, menu),
            "palette": palette_point(variant, palette),
            "verify": verify_point(widget, verify),
            "macos_bundle": bundle_point(widget, bundle),
            "linux_package": package_point(widget, package),
        }
        intent = None
        entry = registry_widgets.get(widget) if registry is not None else None
        if entry is not None:
            line = entry_line(registry_src, widget)
            reason = entry.get("reason")
            intent = {
                "ships": sorted(set(entry["ships"])),
                "reason": reason.strip() if reason is not None else None,
                "evidence": evidence_sorted([(REGISTRY, line)]),
            }
        reports.append({"widget": widget, "intent": intent, "points": points})

        crate_evidence = evidence_of(crate)
        package_evidence = [(cargo.path, package_line)] if cargo.ok else []
        for target, source in (
            ("launch", helper),
            ("command", commands),
            ("menu", menu),
            ("palette", palette),
            ("verify", verify),
        ):
            if points[target]["state"] == "absent":
                add("missing-registration", widget, target, package_evidence + [(source.path, source.anchor)])
        for target, source in (
            ("command", command),
            ("palette", palette),
            ("verify", verify),
            ("macos_bundle", bundle),
        ):
            point = points[target]
            if point["state"] == "partial":
                add(
                    "incomplete-registration",
                    widget,
                    target,
                    evidence_of(point) + [(source.path, source.anchor)],
                    point["missing"],
                )
        if crate["state"] != "unknown" and crate["binary"] != widget:
            add("name-mismatch", widget, "crate", crate_evidence)
        if launch["state"] == "present":
            for target in ("macos_bundle", "linux_package"):
                point = points[target]
                if point["state"] != "unknown" and point["name"] is not None and launch["bundled"] is not None:
                    if point["name"] != launch["bundled"]:
                        add("name-mismatch", widget, target, evidence_of(launch) + evidence_of(point))
            if crate["state"] != "unknown" and crate["accepts_repo"] != launch["repo_arg"]:
                add("repo-arg-mismatch", widget, "launch", crate_evidence + [(helper.path, helper.anchor)])

        if registry is None:
            continue
        if entry is None:
            add("no-intent", widget, "registry", crate_evidence + [(registry_src.path, registry_src.anchor)])
            continue
        line = entry_line(registry_src, widget)
        ships = intent["ships"]
        for platform, point_name, script in (
            ("linux", "linux_package", package),
            ("macos", "macos_bundle", bundle),
        ):
            point = points[point_name]
            if point["state"] == "unknown":
                continue
            shipped = point["state"] == "present" or (platform == "macos" and point["state"] == "partial")
            if platform in ships and not shipped:
                add("ship-missing", widget, platform, [(REGISTRY, line), (script.path, script.anchor)])
            if platform not in ships and shipped:
                add("ship-unintended", widget, platform, [(REGISTRY, line)] + evidence_of(point))
        if ships != list(PLATFORMS):
            reason = intent["reason"]
            for platform in PLATFORMS:
                if platform in ships:
                    continue
                if not reason:
                    add("os-difference-unexplained", widget, platform, [(REGISTRY, line)])
                else:
                    explained.append(
                        {
                            "rule": "os-difference",
                            "widget": widget,
                            "target": platform,
                            "reason": reason,
                            "evidence": evidence_sorted([(REGISTRY, line)]),
                        }
                    )

    if registry is not None and widgets_src.ok:
        for key in sorted(registry_widgets):
            if key not in names:
                add("stale-intent", key, "registry", [(REGISTRY, entry_line(registry_src, key))])

    findings.sort(key=lambda f: (f["widget"], f["rule"], f["target"]))
    explained.sort(key=lambda f: (f["widget"], f["rule"], f["target"]))
    sources.sort(key=lambda s: s.path)
    unknown = any(not s.ok for s in sources)
    exit_code = 2 if unknown else (1 if findings else 0)
    return {
        "schema": 1,
        "scope": SCOPE,
        "status": ("consistent", "inconsistent", "unknown")[exit_code],
        "exit": exit_code,
        "sources": [{"path": s.path, "state": "ok" if s.ok else "unknown", "error": s.error} for s in sources],
        "widgets": reports,
        "findings": findings,
        "explained": explained,
    }


def where(evidence):
    return ", ".join("%s:%d" % (item["path"], item["line"]) for item in evidence)


def render_text(report):
    unknown = [s for s in report["sources"] if s["state"] == "unknown"]
    lines = [
        SCOPE,
        "sources: %d ok, %d unknown" % (len(report["sources"]) - len(unknown), len(unknown)),
    ]
    for source in unknown:
        lines.append("  unknown %s: %s" % (source["path"], source["error"]))
    names = [w["widget"] for w in report["widgets"]]
    lines.append("widgets: %d (%s)" % (len(names), ", ".join(names)))
    lines.append("explained differences: %d" % len(report["explained"]))
    for item in report["explained"]:
        lines.append(
            "  %s %s: %s — %s (%s)"
            % (item["widget"], item["target"], item["rule"], item["reason"], where(item["evidence"]))
        )
    lines.append("unexplained differences: %d" % len(report["findings"]))
    for item in report["findings"]:
        parts = " (%s)" % ", ".join(item["parts"]) if item["parts"] else ""
        lines.append(
            "  %s %s: %s%s (%s)" % (item["widget"], item["target"], item["rule"], parts, where(item["evidence"]))
        )
    lines.append("result: " + report["status"])
    return "\n".join(lines) + "\n"


def main(argv):
    repo = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    as_json = False
    for arg in argv:
        if arg == "--json":
            as_json = True
        elif arg.startswith("--repo="):
            repo = os.path.abspath(arg[len("--repo="):])
        else:
            sys.stderr.write("usage: check-widget-registration.py [--repo=DIR] [--json]\n")
            return 2
    report = build(repo)
    if as_json:
        out = json.dumps(report, indent=2, ensure_ascii=False, sort_keys=True) + "\n"
    else:
        out = render_text(report)
    sys.stdout.buffer.write(out.encode("utf-8"))
    sys.stdout.flush()
    return report["exit"]


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
