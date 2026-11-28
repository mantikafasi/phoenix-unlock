// What Electron Discord draws Japanese with on Windows: Chromium's fallback in English, and the
// first installed font of its Japanese CSS stack in Japanese.
const FONTS: Record<string, string[]> = {
  english: ["Noto Sans JP"],
  japanese: ["Meiryo", "MS PGothic", "Noto Sans"],
};

const fonts = phoenix.fonts;

if (!fonts) {
  phoenix.log.info("japanese-font: this Discord build's fonts can't be changed");
} else {
  const apply = (match: string) => fonts.set({ japanese: FONTS[match] ?? [] });

  const settings = phoenix.settings.define({
    match: {
      type: "select",
      title: "Match Electron Discord in",
      description: "Windows only. Text already on screen changes once Discord draws it again",
      options: [
        { value: "english", label: "English (Noto Sans JP)" },
        { value: "japanese", label: "Japanese (Meiryo)" },
        { value: "off", label: "Off (Yu Gothic UI)" },
      ],
      default: "english",
      onChange: (value) => apply(value),
    },
  });

  apply(settings.match);
}
