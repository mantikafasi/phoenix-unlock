const fonts = phoenix.fonts;

if (!fonts?.supported()) {
  phoenix.log.info("font-changer: this Discord build's fonts can't be changed");
} else {
  const installed = fonts.list?.();
  type Slot = "main" | "mono";
  const picked = (slot: Slot) => phoenix.storage.get<string>(slot) ?? "";
  const apply = () => fonts.set({ main: picked("main"), mono: picked("mono") });

  // The setting holds the search; the font picked from its matches is stored.
  const search = async (slot: Slot, query: string) => {
    const q = query.trim().toLowerCase();
    if (!q) {
      phoenix.storage.delete(slot);
      return apply();
    }
    if (!installed) {
      phoenix.storage.set(slot, query.trim());
      return apply();
    }
    const exact = installed.find((name) => name.toLowerCase() === q);
    const matches = exact ? [exact] : installed.filter((name) => name.toLowerCase().includes(q));
    if (matches.length === 0) {
      await phoenix.ui.choose({ title: "No fonts found", message: query, options: ["OK"] });
      return;
    }
    let font = matches[0];
    if (matches.length > 1) {
      const shown = matches.slice(0, 30);
      const index = await phoenix.ui.choose({ title: `${matches.length} fonts`, options: shown });
      if (index < 0) return;
      font = shown[index];
    }
    phoenix.storage.set(slot, font);
    apply();
  };

  phoenix.settings.define({
    main: {
      type: "text",
      title: "Main font",
      description: "Font name to search for, empty for default",
      default: "",
      onChange: (query) => void search("main", query),
    },
    mono: {
      type: "text",
      title: "Code font",
      description: "Font name to search for, empty for default",
      default: "",
      onChange: (query) => void search("mono", query),
    },
  });

  apply();
}
