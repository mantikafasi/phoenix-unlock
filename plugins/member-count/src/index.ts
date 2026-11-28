// Like Vencord's MemberCount: a server's online and total members at the
// bottom of its tooltip in the server list, a green dot then a grey ring.
//
// The counts of every server come in one request, the user's server list with
// `with_counts`. Discord only refreshes them every few minutes, so neither does
// this.

/** A server as the list gives it, with its counts. */
interface Counted {
  id: string;
  approximate_member_count?: number;
  approximate_presence_count?: number;
}

interface Counts {
  members: number;
  online: number;
}

/** How long the counts are kept before the next tooltip asks for new ones. */
const FRESH_MS = 5 * 60_000;
/**
 * The least time between two requests. A server missing from the counts asks
 * again after it, since the user switched accounts or joined it.
 */
const RETRY_MS = 15_000;

/** Discord's online and offline status colors. */
const ONLINE = 0xff23a55a;
const OFFLINE = 0xff80848e;
const DOT =
  '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><circle cx="12" cy="12" r="7"/></svg>';
const RING =
  '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path fill-rule="evenodd" d="M12 5a7 7 0 1 0 0 14a7 7 0 1 0 0-14zm0 2.6a4.4 4.4 0 1 1 0 8.8a4.4 4.4 0 1 1 0-8.8z"/></svg>';

/**
 * A no-break space. French takes it too, since Discord's font draws the narrow
 * one with no width.
 */
const NBSP = String.fromCharCode(0xa0);

/** Thousands separators by language, for the user's Discord language. */
const SEPARATORS: Record<string, string> = {
  fr: NBSP,
  cs: NBSP,
  fi: NBSP,
  hu: NBSP,
  no: NBSP,
  pl: NBSP,
  ru: NBSP,
  sv: NBSP,
  uk: NBSP,
  bg: NBSP,
  da: ".",
  de: ".",
  el: ".",
  es: ".",
  hr: ".",
  id: ".",
  it: ".",
  lt: ".",
  nl: ".",
  pt: ".",
  ro: ".",
  tr: ".",
  vi: ".",
};

/** Every account's servers seen, so switching back shows them at once. */
const counts = new Map<string, Counts>();
let separator = ",";
/** When the counts last came, and when they were last asked for. */
let fetchedAt = 0;
let askedAt = 0;
let loading: Promise<void> | null = null;

const grouped = (n: number) => String(n).replace(/\B(?=(\d{3})+(?!\d))/g, separator);

const tooltip = phoenix.ui.addServerTooltip?.({
  render({ guildId }) {
    const found = counts.get(guildId);
    const now = Date.now();
    if ((!found || now - fetchedAt > FRESH_MS) && now - askedAt > RETRY_MS) void refresh();
    if (!found) return null;
    return [
      { icons: [{ svg: DOT, color: ONLINE }] },
      { text: grouped(found.online), color: ONLINE },
      // A wider gap between the two counts than within each.
      { text: "  " },
      { icons: [{ svg: RING, color: OFFLINE }] },
      { text: grouped(found.members), color: OFFLINE },
    ];
  },
});

function refresh(): Promise<void> {
  loading ??= (async () => {
    askedAt = Date.now();
    try {
      // Discord's language is in the user settings, not in the cached user, and
      // each account has its own.
      const settings = await phoenix.discord.get("/users/@me/settings");
      const language = (settings.ok && settings.json<{ locale?: string }>()?.locale) || "";
      separator = SEPARATORS[language.split("-")[0]] ?? ",";
      const res = await phoenix.discord.get("/users/@me/guilds?with_counts=true");
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const list = res.json<Counted[]>();
      if (!Array.isArray(list)) throw new Error("unexpected server list");
      const counted = list.filter((guild) => typeof guild.approximate_member_count === "number");
      for (const guild of counted) {
        counts.set(guild.id, {
          members: guild.approximate_member_count!,
          online: guild.approximate_presence_count ?? 0,
        });
      }
      fetchedAt = Date.now();
      // Ready before each tooltip first opens, rather than from its second time.
      for (const guild of counted) tooltip?.update?.(guild.id);
    } catch (err) {
      phoenix.log.warn(`member-count: ${err}`);
    } finally {
      loading = null;
    }
  })();
  return loading;
}

if (tooltip) {
  // Loaded with Discord, this runs before Discord's first request, which the
  // counts' request needs. The answer to that one starts it again.
  const first = phoenix.discord.onResponse({}, () => {
    first();
    if (!counts.size) void refresh();
  });
  void refresh();
} else {
  phoenix.log.info("member-count: this Sunbird can't add to server tooltips");
}
