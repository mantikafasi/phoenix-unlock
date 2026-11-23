#!/usr/bin/env python3
"""Extract Phoenix (Discord native Rust Android) UI & bridge schema from base.apk.

Generates both machine-readable JSON (`docs/phoenix-<ver>.json`) and a concise
Markdown lookup reference (`docs/phoenix-<ver>.md`) covering:
  - All ViewKinds (`yq.<clinit>`)
  - All prop/event wire keys (`lh/c2`) + overlay-specific props (`qo.s` / `overlays/d1`)
  - All `hh.jb` host module IDs, method IDs (`Module.method`), and `@event` IDs
  - All `res://ic_mana_*` drawable icons in `base.apk`
  - Known `accessibilityId` patterns referenced in phoenix-unlock

Zero external dependencies (Python stdlib only).
"""

from __future__ import annotations

import argparse
import json
import re
import struct
import zipfile
from collections import defaultdict
from pathlib import Path


def read_uleb128(buf: bytes, off: int) -> tuple[int, int]:
    result = 0
    shift = 0
    while True:
        byte = buf[off]
        off += 1
        result |= (byte & 0x7F) << shift
        if not (byte & 0x80):
            return result, off
        shift += 7


def parse_dex_strings(dex: bytes) -> list[str]:
    s_size, s_off = struct.unpack_from("<II", dex, 56)
    strings: list[str] = []
    for i in range(s_size):
        data_off = struct.unpack_from("<I", dex, s_off + i * 4)[0]
        _, off = read_uleb128(dex, data_off)
        end = dex.index(0, off)
        strings.append(dex[off:end].decode("utf-8", "replace"))
    return strings


def extract_class_method_strings(
    dex: bytes, strings: list[str], target_classes: set[str]
) -> dict[str, dict[str, list[str]]]:
    t_size, t_off = struct.unpack_from("<II", dex, 64)
    m_size, m_off = struct.unpack_from("<II", dex, 88)
    c_size, c_off = struct.unpack_from("<II", dex, 96)

    def get_type(idx: int) -> str:
        return strings[struct.unpack_from("<I", dex, t_off + idx * 4)[0]]

    def get_method_name(idx: int) -> str:
        _, _, nidx = struct.unpack_from("<HHI", dex, m_off + idx * 8)
        return strings[nidx]

    out: dict[str, dict[str, list[str]]] = {}
    for ci in range(c_size):
        cidx, _, _, _, _, _, cdata_off, _ = struct.unpack_from(
            "<IIIIIIII", dex, c_off + ci * 32
        )
        if cdata_off == 0:
            continue
        cname = get_type(cidx)
        if cname not in target_classes:
            continue
        o = cdata_off
        sf, o = read_uleb128(dex, o)
        if_, o = read_uleb128(dex, o)
        dm, o = read_uleb128(dex, o)
        vm, o = read_uleb128(dex, o)
        for _ in range(sf + if_):
            _, o = read_uleb128(dex, o)
            _, o = read_uleb128(dex, o)
        methods: dict[str, list[str]] = {}
        midx = 0
        for idx in range(dm + vm):
            if idx == dm:
                midx = 0
            diff, o = read_uleb128(dex, o)
            midx += diff
            _, o = read_uleb128(dex, o)
            code_off, o = read_uleb128(dex, o)
            if code_off == 0:
                continue
            insns_size = struct.unpack_from("<I", dex, code_off + 12)[0]
            insns_off = code_off + 16
            k = 0
            mstrs: list[str] = []
            while k < insns_size:
                op = dex[insns_off + k * 2]
                if op == 0x1A and k + 1 < insns_size:
                    sidx = struct.unpack_from("<H", dex, insns_off + (k + 1) * 2)[0]
                    if sidx < len(strings):
                        mstrs.append(strings[sidx])
                    k += 2
                elif op == 0x1B and k + 2 < insns_size:
                    sidx = struct.unpack_from("<I", dex, insns_off + (k + 1) * 2)[0]
                    if sidx < len(strings):
                        mstrs.append(strings[sidx])
                    k += 3
                else:
                    k += 1
            if mstrs:
                methods[get_method_name(midx)] = mstrs
        out[cname] = methods
    return out


def extract_accessibility_ids(repo_root: Path) -> list[str]:
    patterns = set()
    scan_dirs = [repo_root / "crates" / "runtime" / "src", repo_root / "plugins"]
    aid_re = re.compile(
        r'"((?:composer\.[a-z0-9._:-]+|message\.[a-z0-9._:-]+|search\.[a-z0-9._:-]+|settings\.[a-z0-9._:-]+|profile-[a-z0-9._:-]+|expression\.[a-z0-9._:-]+|phoenix\.[a-z0-9._:{}-]+|main-tabs|suggestions|composer|send|Record Voice Message))"'
    )
    for base in scan_dirs:
        if not base.exists():
            continue
        for p in base.rglob("*"):
            if p.suffix not in (".rs", ".ts"):
                continue
            txt = p.read_text("utf-8", errors="ignore")
            for m in aid_re.finditer(txt):
                patterns.add(m.group(1))
    return sorted(patterns)


def load_props_table(repo_root: Path) -> list[str]:
    props_in = repo_root / "crates" / "ui" / "src" / "props.in"
    if props_in.exists():
        text = props_in.read_text("utf-8")
        return re.findall(r'"([^"]+)"', text)
    return []


WIRE_TYPE_HINTS: dict[str, list[str]] = {
    "float (Value::F64)": [
        "opacity", "scrimOpacity", "width", "height", "minWidth", "maxWidth",
        "minHeight", "maxHeight", "radius", "radiusTopLeft", "radiusTopRight",
        "radiusBottomLeft", "radiusBottomRight", "fontSize", "lineHeight",
        "gap", "gapX", "gapY", "flex", "scale", "scaleX", "scaleY",
        "translateX", "translateY", "rotation", "elevation", "borderWidth",
        "hitSlop",
    ],
    "rect (Value::Rect([l,t,r,b]))": [
        "padding", "margin", "activePadding", "activeMargin",
        "rangeInsets", "scrollIndicatorInsets",
    ],
    "color (Value::I64/I32 ARGB)": [
        "color", "background", "activeBackground", "borderColor", "tint",
        "scrimColor", "navScrimColor", "handleColor", "placeholderColor",
        "focusBorderColor", "minTrackTint", "maxTrackTint", "thumbTint",
        "elevationColor", "bg", "bgBorder",
    ],
    "int (Value::I32)": [
        "fontWeight", "maxLines", "index", "first", "seq", "expandSeq",
    ],
    "bool (Value::Bool)": [
        "hidden", "clip", "selectable", "multiline", "secure", "disabled",
        "passthrough", "announcePolite", "dismissible", "expanded",
        "handleHidden", "underSheets", "lineHeightExact", "includeFontPadding",
    ],
}

OVERLAY_PROPS: dict[str, dict[str, str]] = {
    "banner": {
        "edge": '"top" | "bottom" (top auto-pads status bar + cutout)',
        "passthrough": "bool (false = card intercepts taps, outside passes through)",
        "announce": "string (accessibility announcement)",
        "announcePolite": "bool",
    },
    "sheet": {
        "dismissible": "bool",
        "expanded": "bool",
        "expandSeq": "i32",
        "handleColor": "ARGB color",
        "handleHidden": "bool",
        "handleOverlay": "bool",
        "sheetClip": "bool",
        "sheetMaxHeight": "float",
        "scrimColor": "ARGB color",
        "scrimOpacity": "float",
        "background": "ARGB color",
        "radius / radiusTopLeft": "float",
        "on": '["dismiss"]',
    },
    "popover": {
        "placement": '"top" | "bottom" | "left" | "right" | "auto"',
        "align": '"start" | "center" | "end"',
        "edgeInset": "float",
        "offset": "float",
        "passthrough": "bool",
        "underSheets": "bool",
    },
    "modal": {
        "dismissible": "bool",
        "scrim": "bool",
        "scrimColor": "ARGB color",
        "scrimOpacity": "float",
        "backdropBlur": "float",
    },
}


def build_markdown(schema: dict) -> str:
    lines: list[str] = []
    ver = schema["versionCode"]
    lines.append(f"# Phoenix Android Schema Reference (versionCode `{ver}`)")
    lines.append("")
    lines.append(
        "> Auto-generated from `base.apk` by `scripts/extract-schema.py`. "
        "Use this instead of re-disassembling `classes.dex`."
    )
    lines.append("")

    # ViewKinds
    lines.append(f"## 1. ViewKinds ({len(schema['viewKinds'])})")
    lines.append("")
    lines.append(", ".join(f"`{v}`" for v in schema["viewKinds"]))
    lines.append("")

    # Overlay props
    lines.append("## 2. Native Overlay View Props")
    lines.append("")
    for kind, props in schema["overlayProps"].items():
        lines.append(f"### `{kind}`")
        for k, desc in props.items():
            lines.append(f"- `{k}`: {desc}")
        lines.append("")

    # Wire types
    lines.append("## 3. Wire Type Cheat-Sheet (Avoid Kotlin `ClassCastException`)")
    lines.append("")
    for wtype, names in schema["wireTypeHints"].items():
        lines.append(f"- **{wtype}**: " + ", ".join(f"`{n}`" for n in names))
    lines.append("")

    # Host modules
    lines.append(
        f"## 4. Host Modules & Methods (`hh.jb` — {len(schema['hostModules'])} modules, "
        f"{sum(len(v['methods']) for v in schema['hostModules'].values())} methods)"
    )
    lines.append("")
    lines.append("Call via `phoenix.bridge.invoke(moduleId, methodId, args, body)`.")
    lines.append("")
    lines.append("| Module | ID | Methods (`name: id`) |")
    lines.append("| --- | ---: | --- |")
    for mod, info in sorted(schema["hostModules"].items(), key=lambda kv: kv[1]["id"]):
        meths = ", ".join(
            f"`{m}: {mid}`" for m, mid in sorted(info["methods"].items(), key=lambda x: x[1])
        )
        lines.append(f"| `{mod}` | `{info['id']}` | {meths or '—'} |")
    lines.append("")

    # Host events
    lines.append(f"## 5. Host Dispatch Events (`@event` — {len(schema['hostEvents'])})")
    lines.append("")
    ev_list = ", ".join(
        f"`{k}: {v}`" for k, v in sorted(schema["hostEvents"].items(), key=lambda x: x[1])
    )
    lines.append(ev_list)
    lines.append("")

    # Accessibility IDs
    lines.append(f"## 6. Known `accessibilityId` Patterns ({len(schema['accessibilityIds'])})")
    lines.append("")
    lines.append(", ".join(f"`{a}`" for a in schema["accessibilityIds"]))
    lines.append("")

    # Icons summary
    icons = schema["manaIcons"]
    lines.append(f"## 7. Mana Icons (`res://ic_mana_*` — {len(icons)} total)")
    lines.append("")
    lines.append(
        "Full list is in `phoenix-" + str(ver) + ".json` under `manaIcons`. "
        "Commonly used icons:"
    )
    lines.append("")
    common = [
        ic for ic in icons
        if any(
            k in ic
            for k in (
                "puzzle", "x_small", "checkmark", "angle_brackets",
                "image_file", "eye", "copy", "trash", "pencil",
                "arrow_small", "circle_information", "bell", "sparkle",
            )
        )
    ][:40]
    lines.append(", ".join(f"`res://{ic}`" for ic in common))
    lines.append("")
    return "\n".join(lines)


def main() -> None:
    repo_root = Path(__file__).resolve().parent.parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "apk",
        nargs="?",
        default=str(repo_root.parent / "arm64" / "dl" / "base.apk"),
        help="Path to Discord Phoenix base.apk",
    )
    parser.add_argument(
        "--version-code",
        default="356207",
        help="Discord versionCode label for output filenames",
    )
    parser.add_argument(
        "--out-dir",
        default=str(repo_root / "docs"),
        help="Output directory for JSON and Markdown reference files",
    )
    args = parser.parse_args()

    apk_path = Path(args.apk)
    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

    with zipfile.ZipFile(apk_path) as zf:
        dex = zf.read("classes.dex")
        mana_icons = sorted(
            {
                Path(name).stem
                for name in zf.namelist()
                if "/ic_mana_" in name and name.startswith("res/drawable")
            }
        )

    strings = parse_dex_strings(dex)
    cls_strs = extract_class_method_strings(
        dex,
        strings,
        {
            "Lcom/discord/nativeapp/modules/yq;",
            "Lcom/discord/nativeapp/modules/qo;",
            "Lcom/discord/nativeapp/modules/overlays/d1;",
        },
    )

    # Extract ViewKinds from yq.<clinit> (filter out helper strings like getBytes(...))
    raw_kinds = cls_strs.get("Lcom/discord/nativeapp/modules/yq;", {}).get("<clinit>", [])
    view_kinds = [
        s for s in raw_kinds if re.fullmatch(r"[a-z][A-Za-z0-9_]*", s)
    ]

    # Parse hh.jb modules, methods, and @events
    mod_re = re.compile(r"^([A-Z][A-Za-z0-9_]+) (\d+)$")
    meth_re = re.compile(r"^([A-Z][A-Za-z0-9_]+)\.([A-Za-z0-9_]+) (\d+)$")
    ev_re = re.compile(r"^(@[A-Za-z0-9_]+) (\d+)$")

    modules: dict[str, dict] = {}
    methods_by_mod: dict[str, dict[str, int]] = defaultdict(dict)
    events: dict[str, int] = {}

    for s in strings:
        if m := meth_re.match(s):
            methods_by_mod[m.group(1)][m.group(2)] = int(m.group(3))
        elif m := ev_re.match(s):
            events[m.group(1)] = int(m.group(2))

    # Filter module names to real modules (either have methods or known standalone modules)
    non_modules = {"Pixel", "Top"}
    for s in strings:
        if m := mod_re.match(s):
            name, mid = m.group(1), int(m.group(2))
            if name in non_modules:
                continue
            if name in methods_by_mod or mid in (
                1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18,
                19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34,
                35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50,
                51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63,
            ):
                modules[name] = {"id": mid, "methods": dict(sorted(methods_by_mod.get(name, {}).items()))}

    for mod, meths in methods_by_mod.items():
        if mod not in modules:
            modules[mod] = {"id": -1, "methods": dict(sorted(meths.items()))}

    props_table = load_props_table(repo_root)
    aids = extract_accessibility_ids(repo_root)

    schema = {
        "versionCode": args.version_code,
        "viewKinds": view_kinds,
        "propCount": len(props_table),
        "props": props_table,
        "wireTypeHints": WIRE_TYPE_HINTS,
        "overlayProps": OVERLAY_PROPS,
        "hostModules": dict(sorted(modules.items(), key=lambda kv: kv[1]["id"])),
        "hostEvents": dict(sorted(events.items(), key=lambda kv: kv[1])),
        "manaIcons": mana_icons,
        "accessibilityIds": aids,
    }

    json_path = out_dir / f"phoenix-{args.version_code}.json"
    md_path = out_dir / f"phoenix-{args.version_code}.md"
    json_path.write_text(json.dumps(schema, indent=2) + "\n", encoding="utf-8")
    md_path.write_text(build_markdown(schema), encoding="utf-8")

    print(
        f"Wrote {json_path} & {md_path}: "
        f"{len(view_kinds)} viewKinds, {len(props_table)} props, "
        f"{len(modules)} hostModules ({sum(len(m['methods']) for m in modules.values())} methods), "
        f"{len(events)} @events, {len(mana_icons)} ic_mana icons, {len(aids)} accessibilityIds"
    )


if __name__ == "__main__":
    main()
