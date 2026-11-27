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
// they are filtered there instead. Discord treats a page shorter than its `limit` as the end of
// the channel and caches that, so a filtered page is topped up from beyond it until it is full
// again (the same fix as no-blocked-messages).
interface HistoryMessage {
   id?: string;
   content: string;
   embeds: Phoenix.Embed[];
}

const visible = (list: HistoryMessage[]) => list.filter((message) => !isBlocked(message));

/** Query parameters of an API path. QuickJS has no URLSearchParams. */
const queryOf = (path: string) => {
   const params: Record<string, string> = {};
   const query = path.split("?")[1];
   if (!query) return params;
   for (const pair of query.split("&")) {
      const [key, value = ""] = pair.split("=");
      if (key) params[decodeURIComponent(key)] = decodeURIComponent(value);
   }
   return params;
};

// The onResponse hook falls back to the original reply after 4 s, so the top-up stops well before.
const TOP_UP_PAGES = 3;
const TOP_UP_BUDGET_MS = 2500;

// `page` filtered and topped up to its original length, or null when that cannot be done in time
// (the unfiltered page is safer than a short one).
async function fullPage(
   channelPath: string,
   direction: "before" | "after",
   page: HistoryMessage[]
): Promise<HistoryMessage[] | null> {
   const started = Date.now();
   let kept = visible(page);
   let raw = page;
   for (let round = 0; round < TOP_UP_PAGES && kept.length < page.length; round++) {
      if (Date.now() - started > TOP_UP_BUDGET_MS) return null;
      // Pages are newest first: older history continues past the last item, newer past the first.
      const anchor = direction === "before" ? raw[raw.length - 1]?.id : raw[0]?.id;
      if (!anchor) return null;
      const res = await phoenix.discord.get(`${channelPath}?limit=100&${direction}=${anchor}`);
      if (!res.ok) return null;
      const more = res.json<unknown>();
      if (!Array.isArray(more)) return null;
      raw = more as HistoryMessage[];
      kept = direction === "before" ? [...kept, ...visible(raw)] : [...visible(raw), ...kept];
      // A short reply is Discord's real end of history, so a short page is correct from here.
      if (raw.length < 100) break;
   }
   if (kept.length < page.length && raw.length === 100) return null;
   // Trimmed so the next page starts right after what Discord was given.
   return direction === "before" ? kept.slice(0, page.length) : kept.slice(-page.length);
}

phoenix.discord.onResponse({ method: "GET", pathSuffix: "/messages" }, async (res) => {
   if (blocked.length === 0 || res.status !== 200) return;
   const list = res.json<unknown>();
   if (!Array.isArray(list)) return;
   const page = list as HistoryMessage[];
   if (visible(page).length === page.length) return;

   const query = queryOf(res.path);
   const limit = Number(query.limit ?? 50);
   // A page already shorter than asked is the real end of history, so filtering it is safe.
   if (page.length < limit) return { body: visible(page) };
   // A jump loads both sides of a message, and a short side would end history there, so it is
   // left whole.
   if (query.around) return;

   const filled = await fullPage(res.path.split("?")[0], query.after ? "after" : "before", page);
   if (filled) return { body: filled };
});
