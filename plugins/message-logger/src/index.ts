const { gateway } = phoenix;

const settings = phoenix.settings.define({
  keepDeleted: {
    type: "switch",
    title: "Keep deleted messages",
    description: "Keep deleted messages, in red",
    default: true,
  },
  keepEdits: {
    type: "switch",
    title: "Keep edit history",
    description: "Show earlier versions under edited messages",
    default: true,
  },
  keepAttachments: {
    type: "switch",
    title: "Keep removed attachments",
    description: "Leave attachments an edit took off the message in place",
    default: true,
  },
  ignoreBots: {
    type: "switch",
    title: "Ignore bots",
    description: "Skip messages sent by bots",
    default: true,
  },
  ignoreUsers: {
    type: "text",
    title: "Ignored users",
    description: "User IDs separated by commas or spaces",
    default: "",
  },
  ignoreChannels: {
    type: "text",
    title: "Ignored channels",
    description: "Channel IDs separated by commas or spaces",
    default: "",
  },
  ignoreGuilds: {
    type: "text",
    title: "Ignored servers",
    description: "Server IDs separated by commas or spaces",
    default: "",
  },
  keepAcrossRestarts: {
    type: "switch",
    title: "Keep history across restarts",
    description: "Save deleted messages and edit history on this device. Turning it off erases what was saved",
    default: false,
    onChange: (on) => (on ? save() : erase()),
  },
  daysToKeep: {
    type: "number",
    title: "Days to keep saved history",
    description: "0 keeps it until newer history pushes it out",
    default: 30,
    onChange: () => save(),
  },
  clearSaved: {
    type: "button",
    title: "Clear saved history",
    onPress: () => clearSaved(),
  },
});

interface Change {
  /// The text before an edit, or the attachments an edit removed.
  text: string;
  removed: boolean;
  at: number;
}

interface Seen {
  content: string;
  attachments: Phoenix.Attachment[];
  changes: Change[];
  deletedAt?: number;
  repainted?: boolean;
  /// The whole message, to show a deleted one again.
  message?: Phoenix.GatewayMessage;
  channel?: string;
  guild?: string;
  author?: string;
}

type Saved = Omit<Seen, "repainted"> & { id: string };

const isDesktop = phoenix.platform !== "android" && phoenix.platform !== "ios";

const EPHEMERAL = 64;
const RED = 0xfff23f43;
const limit = 8192;
const seen = new Map<string, Seen>();

const ids = (list: string) => list.split(/[\s,]+/).filter(Boolean);

const ignoredPlace = (author?: string, channel?: string, guild?: string) =>
  (author !== undefined && ids(settings.ignoreUsers).includes(author)) ||
  (channel !== undefined && ids(settings.ignoreChannels).includes(channel)) ||
  (guild !== undefined && ids(settings.ignoreGuilds).includes(guild));

const ignored = (message: Phoenix.GatewayMessage) =>
  (message.flags & EPHEMERAL) !== 0 ||
  (settings.ignoreBots && message.author?.bot === true) ||
  ignoredPlace(message.author?.id, message.channel_id, message.guild_id);

const remember = (message: Phoenix.GatewayMessage) => {
  let entry = seen.get(message.id);
  if (!entry) {
    entry = { content: message.content, attachments: [], changes: [] };
    seen.set(message.id, entry);
    if (seen.size > limit) seen.delete(seen.keys().next().value!);
  }
  entry.content = message.content;
  entry.attachments = message.attachments ?? entry.attachments;
  entry.channel = message.channel_id ?? entry.channel;
  entry.guild = message.guild_id ?? entry.guild;
  entry.author = message.author?.id ?? entry.author;
  entry.message = message;
  return entry;
};

// Storage rewrites its whole file on every set, so the saved history stays small.
const SAVED = "history:";
const SAVED_LIMIT = 512;
const SAVED_CHARS = 1_000_000;
const DAY = 86_400_000;
let account: string | undefined;

const lastAt = (entry: Seen) => Math.max(entry.deletedAt ?? 0, ...entry.changes.map((change) => change.at));
const hasHistory = (entry: Seen) => entry.deletedAt !== undefined || entry.changes.length > 0;

const save = () => {
  if (!account || !settings.keepAcrossRestarts) return;
  const days = Number.isFinite(settings.daysToKeep) ? Math.max(0, Math.floor(settings.daysToKeep)) : 30;
  const cutoff = days > 0 ? Date.now() - days * DAY : 0;
  let rows = [...seen]
    .filter(([, entry]) => hasHistory(entry) && lastAt(entry) >= cutoff)
    .sort(([, a], [, b]) => lastAt(a) - lastAt(b))
    .slice(-SAVED_LIMIT)
    .map(([id, { repainted: _, ...entry }]): Saved => ({
      ...entry,
      id,
      message: entry.deletedAt === undefined ? undefined : entry.message,
    }));
  while (rows.length && JSON.stringify(rows).length > SAVED_CHARS) rows = rows.slice(Math.ceil(rows.length / 8));
  try {
    if (rows.length) phoenix.storage.set(SAVED + account, rows);
    else phoenix.storage.delete(SAVED + account);
  } catch (err) {
    phoenix.log.warn(`saving history: ${err}`);
  }
};

const erase = () => {
  for (const key of phoenix.storage.keys()) if (key.startsWith(SAVED)) phoenix.storage.delete(key);
};

const isChange = (change: unknown): change is Change =>
  !!change &&
  typeof (change as Change).text === "string" &&
  typeof (change as Change).removed === "boolean" &&
  Number.isFinite((change as Change).at);

// Desktop switches accounts without reloading plugins.
const bind = (id: string | null | undefined) => {
  if (!id || id === account) return;
  if (account !== undefined) seen.clear();
  account = id;
  if (!settings.keepAcrossRestarts) return;
  const rows = phoenix.storage.get<Saved[]>(SAVED + id);
  if (!Array.isArray(rows)) return;
  for (const row of rows) {
    if (!row || typeof row.id !== "string" || seen.has(row.id) || typeof row.content !== "string") continue;
    if (!Array.isArray(row.changes) || !row.changes.every(isChange)) continue;
    if (ignoredPlace(row.author, row.channel, row.guild)) continue;
    seen.set(row.id, {
      content: row.content,
      attachments: Array.isArray(row.attachments) ? row.attachments : [],
      changes: row.changes,
      deletedAt: Number.isFinite(row.deletedAt) ? row.deletedAt : undefined,
      channel: row.channel,
      guild: row.guild,
      author: row.author,
      message: row.message?.id === row.id ? row.message : undefined,
    });
  }
};

const readAccount = () =>
  phoenix.discord
    .me()
    .then((me) => bind(me?.id))
    .catch(() => {});
if (!settings.keepAcrossRestarts) erase();
readAccount();
// READY skips the event registry on mobile, so this only fires on Desktop.
if (gateway.ready) phoenix.on(gateway.ready, (ready) => bind(ready.user?.id));
if (gateway.readySupplemental) phoenix.on(gateway.readySupplemental, readAccount);

const pad = (n: number) => String(n).padStart(2, "0");
const clock = (at: number) => {
  const date = new Date(at);
  return `${pad(date.getHours())}:${pad(date.getMinutes())}`;
};

// Earlier versions under the message, oldest first, in the accessory's muted style.
const history = phoenix.ui.addMessageAccessory({
  priority: 1,
  render({ messageId, currentUserId }) {
    bind(currentUserId);
    const entry = seen.get(messageId);
    if (!settings.keepEdits || !entry?.changes.length) return null;
    const text = entry.changes
      .map((change) =>
        change.removed
          ? `Removed ${change.text} · ${clock(change.at)}`
          : `${change.text || "(empty)"} · edited ${clock(change.at)}`,
      )
      .join("\n");
    // Red with the rest of a deleted message.
    return entry.deletedAt === undefined ? text : { text, color: RED };
  },
});

const deleted = phoenix.ui.addMessageAccessory({
  render({ messageId }) {
    const at = seen.get(messageId)?.deletedAt;
    return at === undefined ? null : { text: `Deleted · ${clock(at)}`, color: RED, tint: RED };
  },
});

// Phoenix still shows a kept deleted message; clearing only drops its red mark.
const clearSaved = () => {
  for (const [id, entry] of [...seen]) {
    if (!hasHistory(entry)) continue;
    seen.delete(id);
    history.update?.(id);
    deleted.update?.(id);
  }
  if (account) phoenix.storage.delete(SAVED + account);
  phoenix.ui.toast("Cleared the saved history");
};

const query = (path: string) =>
  new Map((path.split("?")[1] ?? "").split("&").map((pair) => pair.split("=") as [string, string]));

const newestFirst = (a: Phoenix.GatewayMessage, b: Phoenix.GatewayMessage) => {
  const [x, y] = [BigInt(a.id), BigInt(b.id)];
  return x === y ? 0 : x > y ? -1 : 1;
};

// The server leaves deleted messages out of history, so kept ones go back in.
const deletedIn = (channelId: string, page: Phoenix.GatewayMessage[], params: Map<string, string>) => {
  if (!settings.keepDeleted) return [];
  const span = page.map((message) => BigInt(message.id));
  const full = page.length >= Number(params.get("limit") || 50);
  const oldest = span.length ? span.reduce((a, b) => (a < b ? a : b)) : undefined;
  const newest = span.length ? span.reduce((a, b) => (a > b ? a : b)) : undefined;
  const before = params.get("before");
  const after = params.get("after");
  let low = full ? oldest : undefined;
  let high: bigint | undefined;
  if (after) [low, high] = [BigInt(after), full ? newest : undefined];
  else if (before) high = BigInt(before);
  else if (params.get("around")) [low, high] = [oldest, newest];
  const shown = new Set(page.map((message) => message.id));
  return [...seen]
    .filter(([id, entry]) => {
      if (entry.deletedAt === undefined || entry.message?.channel_id !== channelId || shown.has(id)) return false;
      const at = BigInt(id);
      return (low === undefined || at > low) && (high === undefined || at < high);
    })
    .map(([, entry]) => entry.message!);
};

// So edits and deletes of messages from before startup are caught too.
phoenix.discord.onResponse({ method: "GET", pathPrefix: "/channels/", pathSuffix: "/messages" }, (res) => {
  if (res.status < 200 || res.status >= 300) return;
  const list = res.json<unknown>();
  if (!Array.isArray(list)) return;
  const page: Phoenix.GatewayMessage[] = [];
  for (const item of list) {
    const msg = item as Phoenix.GatewayMessage;
    if (!msg?.id || typeof msg.content !== "string") continue;
    page.push(msg);
    if (!ignored(msg) && !seen.has(msg.id)) remember(msg);
  }
  if (page.length !== list.length) return;
  const channelId = res.path.split("?")[0].split("/")[2];
  try {
    const kept = deletedIn(channelId, page, query(res.path));
    if (kept.length) return { body: [...page, ...kept].sort(newestFirst) };
  } catch (err) {
    phoenix.log.warn(`restoring deleted messages: ${err}`);
  }
});

phoenix.hook(gateway.messageCreate, {
  before(message) {
    if (!message.id || typeof message.content !== "string" || ignored(message)) return;
    // A create whose nonce names an existing message replaces that message in
    // place, which would erase it without a delete event.
    const nonce = message.nonce === undefined ? undefined : String(message.nonce);
    if (nonce && nonce !== message.id && seen.has(nonce)) delete message.nonce;
    remember(message);
  },
});

phoenix.hook(gateway.messageUpdate, {
  before(message) {
    // Embed-only updates carry no content.
    if (!message.id || !message.channel_id || typeof message.content !== "string") return;
    if (ignored(message)) return;
    const old = seen.get(message.id);
    const at = message.edited_timestamp ? Date.parse(message.edited_timestamp) : Date.now();
    const removed = old?.attachments.filter(
      (attachment) => !message.attachments?.some((kept) => kept.id === attachment.id),
    ) ?? [];
    const changes = old?.changes ?? [];
    const known = changes.length;
    if (old && old.content !== message.content) changes.push({ text: old.content, removed: false, at });
    if (removed.length && settings.keepAttachments) {
      message.attachments = [...(message.attachments ?? []), ...removed];
      changes.push({ text: removed.map((attachment) => attachment.filename).join(", "), removed: true, at });
    }
    remember(message).changes = changes;
    if (old) history.update?.(message.id);
    if (changes.length !== known) save();
  },
});

// Keep deleted messages marked. On desktop, also handle messages loaded from
// local cache before startup (absent from `seen`) as long as their channel and
// server aren't ignored, and trigger a content-free MESSAGE_UPDATE so GPUI
// repaints the row immediately with its native accessory.
const markDeleted = (id: string, channelId?: string, guildId?: string) => {
  let entry = seen.get(id);
  if (!entry) {
    if (!isDesktop) return false;
    if (
      (channelId && ids(settings.ignoreChannels).includes(channelId)) ||
      (guildId && ids(settings.ignoreGuilds).includes(guildId))
    ) {
      return false;
    }
    entry = { content: "", attachments: [], changes: [] };
    seen.set(id, entry);
    if (seen.size > limit) seen.delete(seen.keys().next().value!);
  }
  if (entry.deletedAt === undefined) {
    entry.deletedAt = Date.now();
    deleted.update?.(id);
    if (entry.changes.length) history.update?.(id);
  }
  if (isDesktop && channelId && !entry.repainted) {
    try {
      phoenix.dispatch(gateway.messageUpdate, {
        id,
        channel_id: channelId,
        ...(guildId ? { guild_id: guildId } : {}),
      });
      entry.repainted = true;
    } catch {
      // Runs outside a gateway hook during same-client optimistic delete;
      // the follow-up MESSAGE_DELETE gateway event will trigger the repaint.
    }
  }
  return true;
};

phoenix.hook(gateway.messageDelete, {
  before(message) {
    if (settings.keepDeleted && markDeleted(message.id, message.channel_id, message.guild_id)) {
      save();
      return false;
    }
  },
});

// Desktop drops a message this client deletes before any gateway event, so
// once Discord confirms the delete, the message goes back in and is marked.
if (isDesktop) {
  phoenix.discord.onResponse({ method: "DELETE", pathPrefix: "/channels/", pathIncludes: "/messages/" }, (res) => {
    const [, channelId, , id, rest] = res.path.split("?")[0].split("/").slice(1);
    if (res.status < 200 || res.status >= 300 || rest !== undefined || !settings.keepDeleted) return;
    const message = seen.get(id)?.message;
    if (!message || message.channel_id !== channelId || !markDeleted(id)) return;
    save();
    const { nonce: _, ...restored } = message;
    phoenix.enqueue(gateway.messageCreate, restored);
  });
}

phoenix.hook(gateway.messageDeleteBulk, {
  before(bulk) {
    if (!settings.keepDeleted) return;
    const count = bulk.ids.length;
    bulk.ids = bulk.ids.filter((id) => !markDeleted(id, bulk.channel_id, bulk.guild_id));
    if (bulk.ids.length !== count) save();
    if (!bulk.ids.length) return false;
  },
});
