const { gateway } = phoenix;

const AUTO_MODERATION_ACTION = 24;

const settings = phoenix.settings.define({
  applyToIgnored: {
    type: "switch",
    title: "Also hide ignored users",
    description: "Hide messages from users you marked as Ignored (Discord Ignore), not only Blocked",
    default: true,
  },
  disableNotifications: {
    type: "switch",
    title: "Hide notifications for blocked users",
    description:
      "Also drop new messages from blocked users that stay collapsed. Always on for users hidden completely, as they would otherwise leave unread marks that cannot be found.",
    default: false,
  },
  allowAutoModMessages: {
    type: "switch",
    title: "Keep AutoMod messages",
    description: "Let messages sent by AutoMod through",
    default: true,
  },
  hideBlockedUserReplies: {
    type: "switch",
    title: "Hide replies to blocked users",
    description: "Also hide messages that reply to a blocked user",
    default: false,
  },
  defaultHideUsers: {
    type: "switch",
    title: "Hide blocked users by default",
    description:
      "On: blocked users are hidden completely, and the override list below stays collapsed. Off: blocked users stay collapsed, and the override list is hidden completely.",
    default: true,
  },
  overrideUsers: {
    type: "text",
    title: "Override users",
    description: "Comma separated user IDs that get the opposite of the default above",
    default: "",
  },
});

/** Discord relationship type: BLOCKED */
const REL_BLOCKED = 2;

interface Relationship {
  id: string;
  type: number;
  user_ignored?: boolean;
  user?: { id?: string };
}

interface Suppression {
  suppressed: boolean;
  hide: boolean;
}

const blocked = new Set<string>();
const ignored = new Set<string>();
const CACHE_KEY = "relationships";

const authorId = (msg: { author?: { id?: string } } | null | undefined) =>
  msg?.author?.id;

const overrides = () =>
  new Set(
    settings.overrideUsers
      .split(",")
      .map((id) => id.trim())
      .filter(Boolean),
  );

// Whether a suppressed user's message is dropped completely, rather than left collapsed.
const hides = (userId: string) =>
  settings.defaultHideUsers ? !overrides().has(userId) : overrides().has(userId);

const isSuppressedUser = (userId?: string | null): userId is string => {
  if (!userId) return false;
  if (blocked.has(userId)) return true;
  return settings.applyToIgnored && ignored.has(userId);
};

const suppression = (message: Phoenix.Message): Suppression => {
  const interactionUser = message.interaction?.user.id;
  for (const userId of [authorId(message), interactionUser]) {
    if (isSuppressedUser(userId)) return { suppressed: true, hide: hides(userId) };
  }
  return { suppressed: false, hide: false };
};

const hidden = (message: Phoenix.Message): boolean => {
  if (message.type === AUTO_MODERATION_ACTION && settings.allowAutoModMessages) return false;
  const own = suppression(message);
  if (own.suppressed) return own.hide || settings.disableNotifications;
  if (settings.hideBlockedUserReplies && message.referenced_message) {
    const replied = suppression(message.referenced_message);
    if (replied.suppressed) return replied.hide || settings.disableNotifications;
  }
  return false;
};

const applyRelationship = (rel: Relationship) => {
  const id = rel.id || rel.user?.id;
  if (!id) return;

  if (rel.type === REL_BLOCKED) blocked.add(id);
  else blocked.delete(id);

  if (rel.user_ignored) ignored.add(id);
  else ignored.delete(id);
};

const removeRelationship = (rel: { id?: string; user?: { id?: string } }) => {
  const id = rel.id || rel.user?.id;
  if (!id) return;
  blocked.delete(id);
  ignored.delete(id);
};

const replaceFromList = (list: Relationship[]) => {
  blocked.clear();
  ignored.clear();
  for (const rel of list) applyRelationship(rel);
  try {
    phoenix.storage.set(CACHE_KEY, {
      blocked: [...blocked],
      ignored: [...ignored],
    });
  } catch {
    // storage can fail on first run; live sets still work
  }
};

const readCache = () => {
  try {
    const cached = phoenix.storage.get<{ blocked?: string[]; ignored?: string[] }>(CACHE_KEY);
    if (!cached) return;
    if (Array.isArray(cached.blocked)) for (const id of cached.blocked) blocked.add(id);
    if (Array.isArray(cached.ignored)) for (const id of cached.ignored) ignored.add(id);
  } catch {
    // ignore bad cache
  }
};

const loadRelationships = async () => {
  const res = await phoenix.discord.get("/users/@me/relationships");
  if (!res.ok) throw new Error(`relationships HTTP ${res.status}`);
  const data = res.json<unknown>();
  if (Array.isArray(data)) replaceFromList(data as Relationship[]);
};

// Warm from disk so the first messages after start are filtered before the GET returns.
readCache();
// Desktop has no session until its own first API request, so a load at
// startup fails there. Try again when it first loads a channel's history.
let retry = false;
loadRelationships().catch(() => (retry = true));

// Keep sets in sync when Discord returns the full list again.
phoenix.discord.onResponse(
  { method: "GET", pathPrefix: "/users/@me/relationships" },
  (res) => {
    if (res.status < 200 || res.status >= 300) return;
    const data = res.json<unknown>();
    if (!Array.isArray(data)) return;
    replaceFromList(data as Relationship[]);
  },
);

// Live relationship events (handles exist only if this Phoenix build registered them).
const onRel = (
  name: "relationshipAdd" | "relationshipUpdate" | "relationshipRemove",
  handler: (rel: Relationship) => void,
) => {
  const sig = gateway[name];
  if (!sig) return;
  phoenix.hook(sig, {
    before(payload) {
      handler(payload as Relationship);
    },
  });
};

onRel("relationshipAdd", applyRelationship);
onRel("relationshipUpdate", applyRelationship);
onRel("relationshipRemove", removeRelationship);

// Real-time: drop before Phoenix stores / renders the message.
phoenix.hook(gateway.messageCreate, {
  before(message) {
    if (hidden(message)) return false;
  },
});

phoenix.hook(gateway.messageUpdate, {
  before(message) {
    if (hidden(message)) return false;
  },
});

interface HistoryMessage {
  id?: string;
  author?: { id?: string };
}

const visible = (list: HistoryMessage[]) =>
  list.filter((msg) => !hidden(msg as Phoenix.Message));

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

// Discord treats a history page shorter than its `limit` as the end of the channel and caches
// that, so a filtered page is topped up from beyond it until it is full again. The onResponse
// hook falls back to the original reply after 4 s, so the top-up stops well before that.
const TOP_UP_PAGES = 3;
const TOP_UP_BUDGET_MS = 2500;

/**
 * `page` filtered and topped up to its original length, `null` when that can't be done in time
 * (the unfiltered page is safer than a short one).
 */
const fullPage = async (
  channelPath: string,
  direction: "before" | "after",
  page: HistoryMessage[],
): Promise<HistoryMessage[] | null> => {
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
};

// Channel history / jump-to-message loads.
phoenix.discord.onResponse(
  { method: "GET", pathPrefix: "/channels/", pathSuffix: "/messages" },
  async (res) => {
    if (retry) {
      retry = false;
      loadRelationships().catch((e) =>
        phoenix.log.error("no-blocked-messages: failed to load relationships", e),
      );
    }
    if (res.status < 200 || res.status >= 300) return;
    const list = res.json<unknown>();
    if (!Array.isArray(list)) return;
    const page = list as HistoryMessage[];
    if (visible(page).length === page.length) return;

    const query = queryOf(res.path);
    const limit = Number(query.limit ?? 50);
    // A page already shorter than asked is the real end of history, so filtering it is safe.
    if (page.length < limit) return { body: visible(page) };
    // A jump loads both sides of a message, and a short side would end history there, so it is
    // left whole (blocked messages stay collapsed as Discord shows them).
    if (query.around) return;

    const channelPath = res.path.split("?")[0];
    const filled = await fullPage(channelPath, query.after ? "after" : "before", page);
    if (filled) return { body: filled };
  },
);
