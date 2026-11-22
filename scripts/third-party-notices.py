#!/usr/bin/env python3
"""Writes the third-party notices a binary release ships.

    scripts/third-party-notices.py macos OUT     the omnid binary, aarch64-apple-darwin
    scripts/third-party-notices.py android OUT   the app: omnid-android, aarch64-linux-android,
                                                 and the Java and Kotlin libraries in the APK

The Rust part comes from cargo-about (`cargo install --locked --features cli
cargo-about@0.9.3`). License texts are read from the crates in Cargo's registry, or are the
license's standard text where a crate ships none; only `cargo fetch` goes to the network. The Java and Kotlin part asks Gradle
for the release build's libraries and reads each one's license from its POM, which it asks Gradle
to fetch.
A library under a license this script does not know stops it, rather than going out without
its notice. Write the app's file before `./gradlew assembleRelease`, which packs it.
"""
import json
import os
import re
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ET
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ABOUT = ROOT / "scripts" / "licenses" / "about.toml"
APACHE = ROOT / "scripts" / "licenses" / "Apache-2.0.txt"
TARGETS = {
    "macos": ("omnid", "aarch64-apple-darwin", "the omnid daemon and CLI for Apple-silicon Macs"),
    "android": ("omnid-android", "aarch64-linux-android", "the OMNId Android app"),
}
RULE = "=" * 78


def run(args, cwd=ROOT, env=None):
    res = subprocess.run(args, cwd=cwd, capture_output=True, text=True, env=env)
    if res.returncode != 0:
        sys.exit(f"{' '.join(args[:3])} failed:\n{res.stderr.strip()}")
    return res.stdout


def version():
    meta = json.loads(run(["cargo", "metadata", "--format-version", "1", "--no-deps", "--locked"]))
    members = set(meta["workspace_members"])
    ours = [p for p in meta["packages"] if p["id"] in members]
    return ours[0]["version"], {p["name"] for p in ours}


def rust_section(package, target, ours):
    # cargo-about reads every crate in Cargo.lock, those for other systems too, and must not go
    # to the network itself: fetch them first, checked against Cargo.lock's checksums.
    run(["cargo", "fetch", "--locked", "--quiet"])
    about = json.loads(run([
        "cargo", "about", "generate", "--frozen", "--fail", "--format", "json",
        "-c", str(ABOUT), "-m", str(ROOT / "crates" / package / "Cargo.toml"), "--target", target,
    ]))
    out = [RULE, "Rust crates", RULE, ""]
    for lic in sorted(about["licenses"], key=lambda l: (l["id"], l["text"])):
        users = sorted({(u["crate"]["name"], u["crate"]["version"]) for u in lic["used_by"]
                        if u["crate"]["name"] not in ours})
        if not users:
            continue
        out += [f"{lic['name']} ({lic['id']})", "-" * 78, ""]
        out += wrap("Used by: " + ", ".join(f"{n} {v}" for n, v in users))
        if not lic.get("source_path"):
            # cargo-about found no file for these and gives the license's standard text.
            authors = sorted({re.sub(r"\s*<[^>]*>", "", a).strip() for u in lic["used_by"]
                              if u["crate"]["name"] not in ours for a in u["crate"].get("authors") or []})
            out += [""] + wrap("These crates ship no license file of their own, so the license's "
                               "standard text follows. Their copyright belongs to their authors"
                               + (": " + ", ".join(a for a in authors if a) + "." if authors else "."), indent="")
        out += ["", lic["text"].strip("\n").rstrip(), "", ""]
    return out


def wrap(text, indent="  ", width=78):
    lines, line = [], ""
    for word in text.split(" "):
        if line and len(line) + 1 + len(word) > width:
            lines.append(line)
            line = indent + word
        else:
            line = f"{line} {word}" if line else word
    return lines + [line]


def gradle_libraries():
    tree = run(["./gradlew", "-q", ":app:dependencies", "--configuration", "releaseRuntimeClasspath"],
               cwd=ROOT / "android")
    found = set()
    for m in re.finditer(r"([\w.-]+):([\w.-]+):([\w.+-]+)(?: -> ([\w.+-]+))?", tree):
        group, name, asked, resolved = m.groups()
        found.add((group, name, resolved or asked))
    return sorted(found)


# Resolves each library's POM as an artifact of its own (`group:name:version@pom`) and prints
# where Gradle put it. A build needs only a library's metadata, which for most AndroidX libraries
# is Gradle's own `.module` file, so a cache may well hold no POM for them: CI's restored one did
# not. Asked for this way, Gradle fetches what is missing, from the app's own repositories.
POM_TASK = """
allprojects {
    if (path == ':app') {
        tasks.register('omnidPoms') {
            doLast {
                def wanted = System.getenv('OMNID_POMS').split(' ').collect { project.dependencies.create(it + '@pom') }
                def poms = project.configurations.detachedConfiguration(wanted as Dependency[])
                poms.transitive = false
                poms.resolvedConfiguration.lenientConfiguration.artifacts.each { a ->
                    def id = a.moduleVersion.id
                    println "omnid-pom ${id.group}:${id.name}:${id.version} ${a.file}"
                }
            }
        }
    }
}
"""

# group:name:version -> its POM, as Gradle resolved it.
POMS = {}


def fetch_poms(coords):
    wanted = [":".join(c) for c in coords if ":".join(c) not in POMS]
    if not wanted:
        return
    with tempfile.NamedTemporaryFile("w", suffix=".gradle") as init:
        init.write(POM_TASK)
        init.flush()
        env = dict(os.environ, OMNID_POMS=" ".join(wanted))
        out = run(["./gradlew", "-q", "--no-configuration-cache", "-I", init.name, ":app:omnidPoms"],
                  cwd=ROOT / "android", env=env)
    for line in out.splitlines():
        if line.startswith("omnid-pom "):
            _, coord, path = line.split(" ", 2)
            POMS[coord] = Path(path)


def pom(group, name, ver):
    coord = f"{group}:{name}:{ver}"
    if coord not in POMS:  # a parent POM, asked for when first needed
        fetch_poms([(group, name, ver)])
    if coord not in POMS:
        sys.exit(f"Gradle found no POM for {coord}")
    root = ET.parse(POMS[coord]).getroot()
    for el in root.iter():  # drop the Maven namespace
        el.tag = el.tag.split("}")[-1]
    return root


def pom_license(group, name, ver):
    """The library's license name and URL, from its POM or the nearest parent's; None for a
    module with no code (a BOM, or a multiplatform library's root, whose code is in `-jvm`)."""
    root = pom(group, name, ver)
    if root.findtext("packaging") == "pom":
        return None
    seen = 0
    while root is not None and seen < 8:
        lic = root.find("licenses/license")
        if lic is not None:
            return (lic.findtext("name") or "").strip(), (lic.findtext("url") or "").strip()
        parent = root.find("parent")
        if parent is None:
            break
        root = pom(parent.findtext("groupId"), parent.findtext("artifactId"), parent.findtext("version"))
        seen += 1
    sys.exit(f"no license in the POM of {group}:{name}:{ver} or its parents")


def kind(name, url):
    if re.search(r"apache\.org/licenses/LICENSE-2\.0", url) or re.fullmatch(r"(The )?Apache(-2\.0| (Software )?License,? Version 2\.0)", name):
        return "Apache-2.0"
    if re.search(r"mozilla\.org/MPL/2\.0", url):
        return "MPL-2.0"
    return None


def java_section():
    groups = {"Apache-2.0": [], "MPL-2.0": []}
    libraries = gradle_libraries()
    fetch_poms(libraries)
    for group, name, ver in libraries:
        lic = pom_license(group, name, ver)
        if lic is None:
            continue
        k = kind(*lic)
        if k is None:
            sys.exit(f"{group}:{name}:{ver} is under {lic[0]} ({lic[1]}), which this script does not know: "
                     "add its text here before shipping it")
        groups[k].append(f"{group}:{name} {ver}")
    out = [RULE, "Java and Kotlin libraries", RULE, ""]
    if groups["MPL-2.0"]:
        out += ["Mozilla Public License 2.0", "-" * 78, ""]
        out += wrap("Used by: " + ", ".join(groups["MPL-2.0"]))
        out += ["", "GeckoView is Firefox's engine, unmodified. Its license, and the notices of the",
                "software inside it, are at about:license in OMNId's browser and at",
                "https://www.mozilla.org/MPL/2.0/.", "", ""]
    out += ["Apache License 2.0", "-" * 78, ""]
    out += wrap("Used by: " + ", ".join(groups["Apache-2.0"]))
    out += ["", APACHE.read_text().strip("\n").rstrip(), ""]
    return out


def main():
    if len(sys.argv) != 3 or sys.argv[1] not in TARGETS:
        sys.exit(__doc__.strip().split("\n\n")[1])
    what, out = sys.argv[1], Path(sys.argv[2])
    package, target, label = TARGETS[what]
    ver, ours = version()
    lines = [
        f"Third-party software in OMNId {ver}: {label}",
        "",
        "OMNId is under the MIT License (LICENSE). It is built with the software below, each",
        "under the license shown. A crate that offers a choice of licenses is listed under the",
        "first of MIT, Apache-2.0, BSD and the rest that it offers. The list includes a few",
        "crates that run only while OMNId is compiled.",
        "",
    ]
    lines += rust_section(package, target, ours)
    if what == "android":
        lines += java_section()
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text("\n".join(lines).rstrip() + "\n")
    print(f"{out}: {out.stat().st_size} bytes")


if __name__ == "__main__":
    main()
