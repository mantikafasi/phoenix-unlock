#!/usr/bin/env python3
"""Check a Discord Android split set against what the runtime reads from it.

Before patching a new Discord build: the props `crates/ui-mobile/src/props.in`
names by index, the view kinds of the newest `docs/phoenix-*.json`, and every
host module, method, event and field `crates/runtime-mobile/src/host.rs` looks
up by name in the dex, asked of `patcher --host-names` so it reads the table the
way the runtime does. A shifted prop or a missing name otherwise shows up only
on the phone. Exits 1 with the problems listed when the build does not match.
"""

import argparse
import json
import os
import re
import struct
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
VERSION_CODE = 0x0101021B
HOST_NAME = re.compile(r'=>\s*"([@A-Za-z][A-Za-z0-9_.]*)"')


def version_code(manifest: bytes) -> int:
    _, offset, _ = struct.unpack_from("<HHI", manifest)
    resources: tuple[int, ...] = ()
    while offset < len(manifest):
        kind, header, size = struct.unpack_from("<HHI", manifest, offset)
        if size < header or header < 8 or offset + size > len(manifest):
            raise ValueError("invalid Android manifest chunk")
        if kind == 0x0180:
            resources = struct.unpack_from(f"<{(size - header) // 4}I", manifest, offset + header)
        elif kind == 0x0102:
            extension = offset + header
            start, stride, count = struct.unpack_from("<HHH", manifest, extension + 8)
            if stride < 20 or extension + start + count * stride > offset + size:
                raise ValueError("invalid Android manifest attributes")
            for index in range(count):
                attribute = extension + start + index * stride
                name = struct.unpack_from("<I", manifest, attribute + 4)[0]
                if name < len(resources) and resources[name] == VERSION_CODE:
                    return struct.unpack_from("<I", manifest, attribute + 16)[0]
            break
        offset += size
    raise ValueError("the Android manifest has no versionCode")


def host_names() -> list[str]:
    """Every name host.rs looks up: modules, `Module.method`, `@event` and fields."""
    source = (ROOT / "crates/runtime-mobile/src/host.rs").read_text(encoding="utf-8")
    names = set()
    for name in HOST_NAME.findall(source):
        names.add(name)
        if "." in name:
            names.add(name.split(".", 1)[0])
    return sorted(names)


def found_names(patcher: Path, base: Path, names: list[str]) -> set[str]:
    """The names `base`'s host name table holds, as `patcher --host-names` reads them."""
    if not patcher.is_file():
        raise ValueError(f"no patcher at {patcher}, build it with `cargo build --release -p patcher`")
    lines = subprocess.run(
        [str(patcher), "--host-names", str(base), *names],
        check=True, capture_output=True, text=True,
    ).stdout.splitlines()
    return {name for name, number in (line.rsplit(" ", 1) for line in lines) if number != "-"}


def first_difference(key: str, known: list[str], actual: list[str]) -> list[str]:
    for index, (expected, found) in enumerate(zip(known, actual)):
        if expected != found:
            return [f"{key}[{index}] is {found!r}, the runtime expects {expected!r}"]
    if len(actual) < len(known):
        return [f"{len(actual)} {key}, the runtime expects at least {len(known)}"]
    return []


def newest_schema() -> dict:
    schemas = sorted(ROOT.glob("docs/phoenix-*.json"), key=lambda path: int(re.sub(r"\D", "", path.stem)))
    if not schemas:
        raise ValueError("no docs/phoenix-*.json to compare view kinds with")
    return json.loads(schemas[-1].read_text(encoding="utf-8"))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n", 1)[0])
    parser.add_argument("apks", nargs="+", type=Path, help="base.apk and its config splits")
    parser.add_argument("--version", type=int, help="the versionCode the splits must have")
    parser.add_argument("--report", type=Path, help="also write the result as JSON")
    parser.add_argument("--patcher", type=Path,
                        default=ROOT / "target/release" / ("patcher.exe" if os.name == "nt" else "patcher"),
                        help="the patcher binary, target/release/patcher by default")
    args = parser.parse_args()
    try:
        versions, bases = {}, []
        for path in args.apks:
            with zipfile.ZipFile(path) as apk:
                versions[path] = version_code(apk.read("AndroidManifest.xml"))
                if "classes.dex" in apk.namelist():
                    bases.append(path)
        if len(bases) != 1 or len(set(versions.values())) != 1:
            raise ValueError("pass one base APK and splits of the same Discord version")
        version = versions[bases[0]]
        if args.version is not None and args.version != version:
            raise ValueError(f"the splits are {version}, not {args.version}")

        with tempfile.TemporaryDirectory() as directory:
            subprocess.run(
                [sys.executable, str(ROOT / "scripts/extract-schema.py"), str(bases[0]),
                 "--version-code", str(version), "--out-dir", directory],
                check=True, stdout=subprocess.DEVNULL,
            )
            candidate = json.loads((Path(directory) / f"phoenix-{version}.json").read_text(encoding="utf-8"))
        wanted = host_names()
        present = found_names(args.patcher, bases[0], wanted)

        props = re.findall(r'"([^"]+)"', (ROOT / "crates/ui-mobile/src/props.in").read_text(encoding="utf-8"))
        problems = first_difference("props", props, candidate["props"])
        problems += first_difference("viewKinds", newest_schema()["viewKinds"], candidate["viewKinds"])
        missing = [name for name in wanted if name not in present]
        if missing and len(missing) == len(wanted):
            problems.append(f"patcher finds none of the {len(wanted)} host names, "
                            "so this build's host name table is one it cannot read")
        else:
            problems += [f"the dex has no host name {name!r}" for name in missing]

        report = {
            "versionCode": version,
            "compatible": not problems,
            "props": len(candidate["props"]),
            "appendedProps": candidate["props"][len(props):],
            "viewKinds": len(candidate["viewKinds"]),
            "problems": problems,
        }
        if args.report:
            args.report.parent.mkdir(parents=True, exist_ok=True)
            args.report.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        for problem in problems:
            print(f"incompatible: {problem}", file=sys.stderr)
        if problems:
            return 1
        print(f"Discord {version}: props, view kinds and host names match "
              f"({len(report['appendedProps'])} props appended after props.in)")
        return 0
    except (OSError, ValueError, KeyError, struct.error, zipfile.BadZipFile,
            subprocess.CalledProcessError) as err:
        print(f"compatibility check failed: {err}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
