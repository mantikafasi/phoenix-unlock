// @description Follows your Android system Material You colors automatically
// @version 1.0.9
// @author dim

type PaletteName = "accent1" | "accent2" | "accent3" | "neutral1" | "neutral2";
type Rgb = [r: number, g: number, b: number];
type TonalPalette = { tone: number; rgb: Rgb }[];

interface Roles {
  onSurface: string;
  onSurfaceVariant: string;
  outline: string;
  outlineVariant: string;
  primary: string;
  onPrimary: string;
  primaryContainer: string;
  onPrimaryContainer: string;
  secondaryContainer: string;
  onSecondaryContainer: string;
  tertiary: string;
}

const PLUGIN_ID = "material-you";
const BASES: Phoenix.ThemeBase[] = ["dark", "darker", "midnight", "light"];
const NEUTRAL_RAW = /^(PRIMARY|NEUTRAL)_\d+$/;
const BRAND_RAW = /^(BRAND|BLURPLE)_\d+$/;
const SANS_STYLES = ["Normal", "Medium", "Semibold", "Bold", "ExtraBold"];
const ICONS_BASE_URL =
  "https://raw.githubusercontent.com/dim-ghub/valenced-materialyou/main/themes/material-you";

const SEMANTIC_ROLES: Partial<Record<keyof Roles, string[]>> = {
  onSurface: [
    "TEXT_DEFAULT",
    "TEXT_NORMAL",
    "TEXT_STRONG",
    "HEADER_PRIMARY",
    "MOBILE_TEXT_HEADING_PRIMARY",
    "INTERACTIVE_ACTIVE",
    "INTERACTIVE_HOVER",
    "INTERACTIVE_TEXT_ACTIVE",
    "INTERACTIVE_TEXT_HOVER",
    "ICON_STRONG",
    "CHANNELS_ACTIVE",
  ],
  onSurfaceVariant: [
    "TEXT_MUTED",
    "TEXT_SUBTLE",
    "HEADER_SECONDARY",
    "INTERACTIVE_NORMAL",
    "INTERACTIVE_TEXT_DEFAULT",
    "ICON_DEFAULT",
    "ICON_SUBTLE",
    "CHANNELS_DEFAULT",
    "CHANNEL_ICON",
  ],
  outline: ["INTERACTIVE_MUTED", "ICON_MUTED", "TEXT_PLACEHOLDER", "BORDER_STRONG"],
  outlineVariant: [
    "BORDER_SUBTLE",
    "BORDER_FAINT",
    "BORDER_NORMAL",
    "CONTROL_SECONDARY_BORDER_DEFAULT",
  ],
  primary: [
    "BACKGROUND_BRAND",
    "TEXT_BRAND",
    "TEXT_LINK",
    "ICON_BRAND",
    "CONTROL_BRAND_FOREGROUND",
    "CONTROL_BRAND_FOREGROUND_NEW",
    "CONTROL_PRIMARY_BACKGROUND_DEFAULT",
    "CONTROL_PRIMARY_BACKGROUND_HOVER",
    "CONTROL_PRIMARY_BACKGROUND_ACTIVE",
    "REDESIGN_BUTTON_PRIMARY_BACKGROUND",
    "REDESIGN_BUTTON_PRIMARY_PRESSED_BACKGROUND",
    "BUTTON_FILLED_BRAND_BACKGROUND",
    "MENTION_FOREGROUND",
  ],
  onPrimary: [
    "CONTROL_PRIMARY_TEXT_DEFAULT",
    "CONTROL_PRIMARY_TEXT_HOVER",
    "CONTROL_PRIMARY_TEXT_ACTIVE",
    "REDESIGN_BUTTON_PRIMARY_TEXT",
    "BUTTON_FILLED_BRAND_TEXT",
  ],
  secondaryContainer: [
    "CONTROL_SECONDARY_BACKGROUND_DEFAULT",
    "CONTROL_SECONDARY_BACKGROUND_HOVER",
    "CONTROL_SECONDARY_BACKGROUND_ACTIVE",
    "REDESIGN_BUTTON_SECONDARY_BACKGROUND",
  ],
  onSecondaryContainer: [
    "CONTROL_SECONDARY_TEXT_DEFAULT",
    "CONTROL_SECONDARY_TEXT_HOVER",
    "CONTROL_SECONDARY_TEXT_ACTIVE",
    "REDESIGN_BUTTON_SECONDARY_TEXT",
  ],
};

const TRANSLUCENT_SEMANTIC_ROLES: Record<string, [keyof Roles, number]> = {
  MENTION_BACKGROUND: ["primary", 0.16],
  BACKGROUND_MENTIONED: ["primary", 0.1],
  BACKGROUND_MENTIONED_HOVER: ["primary", 0.14],
  INFO_BOX_BACKGROUND: ["primary", 0.1],
};

function parseHex(hex: string): Rgb {
  const n = Number.parseInt(hex.slice(1, 7), 16);
  return [(n >> 16) & 0xff, (n >> 8) & 0xff, n & 0xff];
}

function toHex([r, g, b]: Rgb): string {
  return `#${[r, g, b]
    .map((c) => Math.round(c).toString(16).padStart(2, "0"))
    .join("")}`;
}

function linearize(c: number): number {
  const s = c / 255;
  return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
}

// Material 3 tone is CIE L*.
function toneOf([r, g, b]: Rgb): number {
  const y = 0.2126 * linearize(r) + 0.7152 * linearize(g) + 0.0722 * linearize(b);
  return y <= 216 / 24389 ? (y * 24389) / 27 : 116 * Math.cbrt(y) - 16;
}

function tonalPalette(shades: string[]): TonalPalette {
  return shades
    .map(parseHex)
    .map((rgb) => ({ tone: toneOf(rgb), rgb }))
    .sort((a, b) => a.tone - b.tone);
}

function at(palette: TonalPalette, tone: number): string {
  const first = palette[0];
  const last = palette[palette.length - 1];
  if (!first || !last) return "#000000";
  if (tone <= first.tone) return toHex(first.rgb);
  if (tone >= last.tone) return toHex(last.rgb);

  for (let i = 1; i < palette.length; i++) {
    const hi = palette[i];
    if (!hi || hi.tone < tone) continue;
    const lo = palette[i - 1];
    if (!lo) continue;

    const mix = (t: number): Rgb => [
      lo.rgb[0] + (hi.rgb[0] - lo.rgb[0]) * t,
      lo.rgb[1] + (hi.rgb[1] - lo.rgb[1]) * t,
      lo.rgb[2] + (hi.rgb[2] - lo.rgb[2]) * t,
    ];

    // RGB blends are non-linear in L*, so binary search for the target tone.
    let min = 0;
    let max = 1;
    for (let j = 0; j < 16; j++) {
      const t = (min + max) / 2;
      if (toneOf(mix(t)) < tone) min = t;
      else max = t;
    }
    return toHex(mix((min + max) / 2));
  }

  return toHex(last.rgb);
}

function withAlpha(hex: string, alpha: number): string {
  return (
    hex +
    Math.round(alpha * 255)
      .toString(16)
      .padStart(2, "0")
  );
}

function rolesOf(base: Phoenix.ThemeBase, p: Record<PaletteName, TonalPalette>): Roles {
  if (base === "light") {
    return {
      onSurface: at(p.neutral1, 10),
      onSurfaceVariant: at(p.neutral2, 30),
      outline: at(p.neutral2, 50),
      outlineVariant: at(p.neutral2, 80),
      primary: at(p.accent1, 40),
      onPrimary: at(p.accent1, 100),
      primaryContainer: at(p.accent1, 90),
      onPrimaryContainer: at(p.accent1, 10),
      secondaryContainer: at(p.accent2, 90),
      onSecondaryContainer: at(p.accent2, 10),
      tertiary: at(p.accent3, 40),
    };
  }
  return {
    onSurface: at(p.neutral1, 90),
    onSurfaceVariant: at(p.neutral2, 80),
    outline: at(p.neutral2, 60),
    outlineVariant: at(p.neutral2, 30),
    primary: at(p.accent1, 80),
    onPrimary: at(p.accent1, 20),
    primaryContainer: at(p.accent1, 30),
    onPrimaryContainer: at(p.accent1, 90),
    secondaryContainer: at(p.accent2, 30),
    onSecondaryContainer: at(p.accent2, 90),
    tertiary: at(p.accent3, 80),
  };
}

function buildColors(
  palettes: Phoenix.SystemPalettes,
  rawTokens: Record<string, string>,
  surfaces: PaletteName,
): Phoenix.ThemeColorValues {
  const p: Record<PaletteName, TonalPalette> = {
    accent1: tonalPalette(palettes.accent1),
    accent2: tonalPalette(palettes.accent2),
    accent3: tonalPalette(palettes.accent3),
    neutral1: tonalPalette(palettes.neutral1),
    neutral2: tonalPalette(palettes.neutral2),
  };

  // Swap Discord's grays and blurple for system colors of the same tone.
  const raw: Record<string, string> = {};
  for (const [name, value] of Object.entries(rawTokens)) {
    const palette: PaletteName | undefined = NEUTRAL_RAW.test(name)
      ? surfaces
      : BRAND_RAW.test(name)
        ? "accent1"
        : undefined;
    if (!palette || !/^#[0-9a-f]{6}/i.test(value)) continue;
    raw[name] = at(p[palette], toneOf(parseHex(value))) + value.slice(7);
  }

  const semantic: Record<string, Record<string, string>> = {};
  for (const base of BASES) {
    const roles = rolesOf(base, p);
    const colors: Record<string, string> = {};

    for (const [role, names] of Object.entries(SEMANTIC_ROLES)) {
      if (!names) continue;
      for (const name of names) colors[name] = roles[role as keyof Roles];
    }
    for (const [name, [role, alpha]] of Object.entries(TRANSLUCENT_SEMANTIC_ROLES)) {
      colors[name] = withAlpha(roles[role], alpha);
    }

    semantic[base] = colors;
  }

  return { raw, semantic };
}

function applyColors(variant: string): void {
  const palettes = phoenix.themes.systemPalettes();
  if (!palettes) {
    phoenix.log.warn("Material You requires Android 12+");
    return;
  }
  const surfaces: PaletteName = variant === "tinted" ? "neutral2" : "neutral1";
  const values = buildColors(palettes, phoenix.themes.tokens().raw, surfaces);
  phoenix.themes.setColors({
    plugin: PLUGIN_ID,
    group: PLUGIN_ID,
    theme: variant,
    values,
  });
}

function applyFont(enabled: boolean): void {
  if (!enabled) {
    if (phoenix.themes.getState().fonts?.families?.systemFont?.plugin === PLUGIN_ID) {
      phoenix.themes.setFonts(null);
    }
    return;
  }
  const source: Phoenix.FontSource = {
    plugin: PLUGIN_ID,
    group: PLUGIN_ID,
    pack: "system",
    variants: [{ path: "sans-serif", weight: 400, italic: false }],
  };
  const families: Record<string, Phoenix.FontSource> = { systemFont: source };
  for (const style of SANS_STYLES) {
    families[`ggsans-${style}`] = source;
  }
  phoenix.themes.setFonts({ families });
}

function applyEmoji(enabled: boolean): void {
  if (!enabled) {
    if (phoenix.themes.getState().emoji?.plugin === PLUGIN_ID) {
      phoenix.themes.setEmoji(null);
    }
    return;
  }
  phoenix.themes.setEmoji({
    plugin: PLUGIN_ID,
    group: PLUGIN_ID,
    pack: "system",
    font: "system",
  });
}

function applyIconList(names: string[]): void {
  const icons: Record<string, string> = {};
  for (const name of names) {
    icons[name] = `${ICONS_BASE_URL}/assets/icons/${name}.png`;
  }
  phoenix.themes.setIcons({
    plugin: PLUGIN_ID,
    group: PLUGIN_ID,
    pack: "symbols",
    icons,
    drawables: {},
  });
}

function applyIcons(enabled: boolean): void {
  if (!enabled) {
    if (phoenix.themes.getState().icons?.plugin === PLUGIN_ID) {
      phoenix.themes.setIcons(null);
    }
    return;
  }
  const cached = phoenix.storage.get<string[]>("iconNames");
  if (Array.isArray(cached) && cached.length > 0) {
    applyIconList(cached);
    return;
  }
  phoenix
    .fetch(`${ICONS_BASE_URL}/icons.json`)
    .then((res) => {
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const map = res.json<Record<string, unknown>>();
      const names = Object.keys(map);
      phoenix.storage.set("iconNames", names);
      if (settings.materialIcons) applyIconList(names);
    })
    .catch((err) => phoenix.log.warn(`Material Symbols icons: ${err}`));
}

const settings = phoenix.settings.define({
  variant: {
    type: "select",
    title: "Surface style",
    description: "Choose neutral or accent-tinted surfaces",
    options: [
      { value: "system", label: "Material You" },
      { value: "tinted", label: "Material You (Tinted)" },
    ],
    default: "system",
    onChange(value) {
      applyColors(value);
    },
  },
  systemFont: {
    type: "switch",
    title: "System font",
    description: "Use your device's system font instead of gg sans",
    default: true,
    onChange(value) {
      applyFont(value);
    },
  },
  systemEmoji: {
    type: "switch",
    title: "System emoji",
    description: "Render standard emoji with your device's emoji font",
    default: true,
    onChange(value) {
      applyEmoji(value);
    },
  },
  materialIcons: {
    type: "switch",
    title: "Material Symbols icons",
    description: "Replace Discord icons with Material Symbols",
    default: true,
    onChange(value) {
      applyIcons(value);
    },
  },
  refresh: {
    type: "button",
    title: "Refresh system colors",
    description: "Re-read your Android wallpaper palette",
    onPress() {
      applyColors(settings.variant);
      phoenix.ui.toast("Refreshed Material You colors");
    },
  },
});

applyColors(settings.variant);
applyFont(settings.systemFont);
applyEmoji(settings.systemEmoji);
applyIcons(settings.materialIcons);

phoenix.onUnload(() => {
  const state = phoenix.themes.getState();
  if (state.colors?.plugin === PLUGIN_ID) phoenix.themes.setColors(null);
  if (state.fonts?.families?.systemFont?.plugin === PLUGIN_ID) phoenix.themes.setFonts(null);
  if (state.icons?.plugin === PLUGIN_ID) phoenix.themes.setIcons(null);
  if (state.emoji?.plugin === PLUGIN_ID) phoenix.themes.setEmoji(null);
});
