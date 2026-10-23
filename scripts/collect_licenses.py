#!/usr/bin/env python3
"""Copy license files for the libraries linked into the handheld port."""

import argparse
import json
import shutil
import subprocess
import sys
from pathlib import Path

TARGET = "aarch64-unknown-linux-gnu"
LICENSE_PREFIXES = ("license", "copying", "notice", "unlicense", "authors")
SKIP_DIRS = {".git", "target", "tests", "test", "examples", "benches", "docs"}


def cargo_metadata(manifest: Path, extra: list[str]) -> dict:
    command = [
        "cargo",
        "metadata",
        "--locked",
        "--format-version",
        "1",
        "--filter-platform",
        TARGET,
        "--manifest-path",
        str(manifest),
        *extra,
    ]
    completed = subprocess.run(command, check=True, capture_output=True)
    return json.loads(completed.stdout.decode("utf-8"))


def reachable(metadata: dict, roots: set[str]) -> dict[str, dict]:
    packages = {package["id"]: package for package in metadata["packages"]}
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    stack = [package_id for package_id, package in packages.items() if package["name"] in roots]
    seen: set[str] = set()
    while stack:
        package_id = stack.pop()
        if package_id in seen or package_id not in nodes:
            continue
        seen.add(package_id)
        for dependency in nodes[package_id]["deps"]:
            kinds = {kind.get("kind") for kind in dependency.get("dep_kinds", [])}
            if kinds and kinds <= {"dev", "build"}:
                continue
            stack.append(dependency["pkg"])
    return {package_id: packages[package_id] for package_id in seen if package_id in packages}


def is_license_file(path: Path) -> bool:
    name = path.name.lower()
    return path.is_file() and name.startswith(LICENSE_PREFIXES) and path.stat().st_size <= 512 * 1024


def license_files(crate_dir: Path) -> list[tuple[Path, str]]:
    found: list[tuple[Path, str]] = []
    seen: set[str] = set()

    def add(path: Path, label: str) -> None:
        if label in seen or not is_license_file(path):
            return
        seen.add(label)
        found.append((path, label))

    if not crate_dir.is_dir():
        return found
    for entry in crate_dir.iterdir():
        if entry.is_file():
            add(entry, entry.name)
        elif entry.is_dir() and entry.name not in SKIP_DIRS:
            nested_files = entry.rglob("*") if entry.name in {"SDL", "opus", "upstream"} else entry.iterdir()
            for nested in nested_files:
                if not nested.is_file():
                    continue
                relative = nested.relative_to(crate_dir)
                if any(part in SKIP_DIRS or part in {"visualtest", "Xcode", "Xcode-iOS"} for part in relative.parts):
                    continue
                add(nested, "__".join(relative.parts))
    return found


def folder_name(name: str, version: str) -> str:
    raw = f"{name}-{version}"
    return "".join(character if character.isalnum() or character in "._-" else "_" for character in raw)


def copy_tree(source: Path, destination: Path) -> None:
    destination.mkdir(parents=True, exist_ok=True)
    for entry in source.rglob("*"):
        if not entry.is_file() or entry.stat().st_size > 512 * 1024:
            continue
        target = destination / entry.relative_to(source)
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(entry, target)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    root = args.root.resolve()
    output = args.out.resolve()
    if output.exists():
        shutil.rmtree(output)
    output.mkdir(parents=True)

    queries = [
        (root / "opennow-core" / "Cargo.toml", ["--no-default-features"], {"opennow-core"}),
        (root / "opennow-handheld" / "Cargo.toml", [], {"opennow-handheld"}),
        (
            root / "opennow-streamer" / "Cargo.toml",
            [],
            {"opennow-streamer-ffi"},
        ),
    ]
    selected: dict[str, dict] = {}
    for manifest, extra, roots in queries:
        selected.update(reachable(cargo_metadata(manifest, extra), roots))

    index: list[str] = []
    project_license = root / "LICENSE"
    for package in sorted(selected.values(), key=lambda item: (item["name"], item["version"])):
        crate_dir = Path(package["manifest_path"]).parent
        destination = output / folder_name(package["name"], package["version"])
        destination.mkdir(parents=True)
        copied = license_files(crate_dir)
        if not copied and project_license.is_file() and crate_dir.is_relative_to(root):
            copied = [(project_license, "LICENSE")]
        for source, label in copied:
            shutil.copyfile(source, destination / label)
        license_name = package.get("license") or "unknown"
        if not copied:
            (destination / "DECLARED.txt").write_text(
                f"{package['name']} {package['version']} declares license: {license_name}\n"
                "No license file was found beside the crate.\n",
                encoding="utf-8",
            )
        index.append(f"{package['name']} {package['version']} {license_name}")

    bundled = root / "third_party" / "licenses"
    if bundled.is_dir():
        for entry in sorted(bundled.iterdir()):
            if entry.is_dir():
                copy_tree(entry, output / entry.name)
                index.append(f"{entry.name} bundled see {entry.name}/")

    (output / "INDEX.txt").write_text(
        "Libraries linked into this port and the license each one declares.\n\n"
        + "\n".join(index)
        + "\n",
        encoding="utf-8",
    )
    print(f"Wrote {len(index)} license entries to {output}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
