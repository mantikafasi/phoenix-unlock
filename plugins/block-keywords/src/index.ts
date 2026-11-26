const settings = phoenix.settings.define({
   blockedWords: {
      type: "text",
      title: "Blocked words",
      description: "Comma-separated list of words to block",
      default: "",
      onChange: () => rebuild(),
   },
   useRegex: {
      type: "switch",
      title: "Use regular expressions",
      description: "Treat each value as a regular expression instead of a whole word (advanced)",
      default: false,
      onChange: () => rebuild(),
   },
   caseSensitive: {
      type: "switch",
      title: "Case sensitive",
      description: "Whether to match upper and lower case separately",
      default: false,
      onChange: () => rebuild(),
   },
});

// Commas inside [...] and {n,m} belong to the pattern, so they are not separators.
function splitPatterns(input: string): string[] {
   return input
      .replace(/\[([^\]]*)\]/g, (m) => m.replace(/,/g, "\u0000"))
      .replace(/\{(\d+,?\d*|,\d+)\}/g, (m) => m.replace(",", "\u0000"))
      .split(",")
      .map((s) => s.replace(/\u0000/g, ",").trim())
      .filter(Boolean);
}

const escape = (text: string) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

let blocked: RegExp[] = [];

function rebuild() {
   const flags = settings.caseSensitive ? "" : "i";
   blocked = [];
   for (const pattern of splitPatterns(settings.blockedWords)) {
      try {
         blocked.push(new RegExp(settings.useRegex ? pattern : `\\b${escape(pattern)}\\b`, flags));
      } catch (error) {
         phoenix.log.warn(`ignoring invalid pattern ${pattern}: ${String(error)}`);
      }
   }
}
rebuild();

const matches = (text: string | null | undefined) => text != null && blocked.some((regex) => regex.test(text));

function isBlocked(message: Pick<Phoenix.Message, "content" | "embeds">): boolean {
   if (blocked.length === 0) return false;
   return (
      matches(message.content) ||
      (message.embeds ?? []).some((embed) => matches(embed.description) || matches(embed.title))
   );
}

// New and edited messages never reach the app.
phoenix.hook(phoenix.gateway.messageCreate, { before: (message) => (isBlocked(message) ? false : undefined) });
phoenix.hook(phoenix.gateway.messageUpdate, { before: (message) => (isBlocked(message) ? false : undefined) });

// Equicord filters Discord's message store. The app's history loads arrive as API responses, so
// they are filtered there instead.
phoenix.discord.onResponse({ method: "GET", pathSuffix: "/messages" }, (res) => {
   if (blocked.length === 0 || res.status !== 200) return;
   const messages = res.json<Phoenix.Message[]>();
   if (!Array.isArray(messages)) return;
   const kept = messages.filter((message) => !isBlocked(message));
   if (kept.length === messages.length) return;
   return { body: kept };
});
