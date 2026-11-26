const settings = phoenix.settings.define({
   blockedWords: {
      type: "text",
      title: "Words not to capitalize",
      description: "Sentences starting with one of these stay as typed, separated by commas",
      default: "http, https, ok",
   },
});

// Equicord splits on ", " only, so "http,ok" never matched. Splitting on commas and trimming
// is the evident intent.
const blocked = (): string[] =>
   settings.blockedWords
      .split(",")
      .map((word) => word.trim().toLowerCase())
      .filter(Boolean);

phoenix.messages.onSend((text) => {
   const words = blocked();
   // The capture group keeps the whitespace between sentences, at the odd indexes.
   const sentences = text.split(/(?<=[.!?]+['")\]]*)(\s+)/);
   const out = sentences
      .map((part, i) => {
         if (i % 2 === 1) return part;
         const lower = part.toLowerCase();
         if (words.some((word) => lower.startsWith(word))) return part;
         return part.charAt(0).toUpperCase() + part.slice(1);
      })
      .join("");
   return out === text ? undefined : out;
});
