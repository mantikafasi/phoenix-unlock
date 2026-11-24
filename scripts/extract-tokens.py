#!/usr/bin/env python3
"""Extract Discord's raw and semantic color token tables from a mobile datamining checkout.

Reads `raw-color-definitions.tsx` and `colors/generated/native/generated-definitions.tsx`
and writes `crates/ui/src/themes/tokens.json` with pre-resolved 32-bit ARGB values for each
of Discord's four base themes (`dark`/ash, `darker`/dark, `light`, `midnight`/onyx).
"""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path

CATEGORY_CODE = {
    "background": "b",
    "text": "t",
    "border": "r",
    "generic": "g",
}

THEME_ORDER = ("ASH", "DARK", "LIGHT", "ONYX")


def parse_hex(value: str) -> tuple[int, bool]:
    """Return `(argb, has_alpha)` for `#RRGGBB` or `#RRGGBBAA`."""
    digits = value.lstrip("#")
    if len(digits) == 6:
        rgb = int(digits, 16)
        return (0xFF000000 | rgb), False
    if len(digits) == 8:
        rgba = int(digits, 16)
        rgb = rgba >> 8
        alpha = rgba & 0xFF
        return ((alpha << 24) | rgb), True
    raise ValueError(f"unsupported hex color: {value}")


def resolve_argb(raw_argb: int, has_alpha: bool, opacity: float) -> int:
    if has_alpha:
        return raw_argb
    alpha = max(0, min(255, round(opacity * 255.0)))
    return (alpha << 24) | (raw_argb & 0x00FFFFFF)


def extract(datamining: Path) -> dict:
    tokens_dir = datamining / "discord_common/js/packages/tokens/colors/generated"
    raw_src = (tokens_dir / "raw-color-definitions.tsx").read_text(encoding="utf-8")
    sem_src = (tokens_dir / "native/generated-definitions.tsx").read_text(encoding="utf-8")

    raw_parsed: dict[str, tuple[int, bool]] = {}
    for m in re.finditer(r'^\s*([A-Z0-9_]+):\s*"(#[0-9a-fA-F]{6,8})"', raw_src, re.M):
        raw_parsed[m.group(1)] = parse_hex(m.group(2))

    vars_map: dict[str, dict] = {}
    for m in re.finditer(r'const (obj\d+) = \{ category: "([^"]+)" \};', sem_src):
        vars_map[m.group(1)] = {"category": m.group(2), "themes": {}}

    for m in re.finditer(
        r'(obj\d+)\[Themes\.([A-Z]+)\] = \{ raw: "([^"]+)", opacity: ([0-9.eE+-]+) \};',
        sem_src,
    ):
        var, theme, raw_name, opacity = m.group(1), m.group(2), m.group(3), float(m.group(4))
        if var in vars_map:
            vars_map[var]["themes"][theme] = (raw_name, opacity)

    semantic: dict[str, list] = {}
    for m in re.finditer(r"obj2\.([A-Z0-9_]+) = (obj\d+);", sem_src):
        name, var = m.group(1), m.group(2)
        info = vars_map.get(var)
        if not info:
            continue
        code = CATEGORY_CODE.get(info["category"], "g")
        values = []
        for theme in THEME_ORDER:
            raw_name, opacity = info["themes"][theme]
            raw_argb, has_alpha = raw_parsed[raw_name]
            values.append(resolve_argb(raw_argb, has_alpha, opacity))
        semantic[name] = [code, *values]

    raw = {name: argb for name, (argb, _) in sorted(raw_parsed.items())}
    return {"raw": raw, "semantic": dict(sorted(semantic.items()))}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("datamining", type=Path, help="Path to discord-mobile-datamining checkout")
    parser.add_argument(
        "-o",
        "--out",
        type=Path,
        default=Path(__file__).resolve().parent.parent / "crates/ui/src/themes/tokens.json",
        help="Output JSON path",
    )
    args = parser.parse_args()

    data = extract(args.datamining)
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(data, separators=(",", ":")), encoding="utf-8")
    print(f"wrote {len(data['raw'])} raw and {len(data['semantic'])} semantic tokens to {args.out}")


if __name__ == "__main__":
    main()
