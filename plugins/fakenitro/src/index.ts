const { gateway } = phoenix;

let removeStreamUnlock: Phoenix.Remover | null = null;
let removeAppearanceUnlock: Phoenix.Remover | null = null;

const syncStreamUnlock = (enabled: boolean) => {
  if (enabled) {
    if (!removeStreamUnlock && phoenix.stream?.unlockQuality) {
      removeStreamUnlock = phoenix.stream.unlockQuality();
    }
  } else if (removeStreamUnlock) {
    removeStreamUnlock();
    removeStreamUnlock = null;
  }
};

const syncAppearanceUnlock = (enabled: boolean) => {
  if (enabled) {
    if (!removeAppearanceUnlock && phoenix.appearance?.unlockNitro) {
      removeAppearanceUnlock = phoenix.appearance.unlockNitro();
    }
  } else if (removeAppearanceUnlock) {
    removeAppearanceUnlock();
    removeAppearanceUnlock = null;
  }
};

const settings = phoenix.settings.define({
  enableStreamQualityBypass: {
    type: "switch",
    title: "Unlock Nitro stream quality",
    description: "Allow streaming in 1080p, 1440p, Source, and 60 FPS without Nitro (Desktop)",
    default: true,
    onChange(enabled) {
      syncStreamUnlock(enabled);
    },
  },
  enableAppearanceBypass: {
    type: "switch",
    title: "Unlock Nitro themes and app icons",
    description: "Allow Nitro color themes and app icons in Settings, Appearance, on this device (Desktop)",
    default: true,
    onChange(enabled) {
      syncAppearanceUnlock(enabled);
    },
  },
  enableStickerBypass: {
    type: "switch",
    title: "Bypass sticker restrictions",
    description: "Send external stickers as links when you don't have Nitro",
    default: true,
  },
  stickerSize: {
    type: "select",
    title: "Sticker size",
    options: [
      { value: "160" },
      { value: "128" },
      { value: "96" },
      { value: "64" },
      { value: "320" },
    ],
  },
  emojiSize: {
    type: "select",
    title: "Emoji size",
    options: [
      { value: "48" },
      { value: "32" },
      { value: "64" },
      { value: "96" },
      { value: "128" },
      { value: "160" },
    ],
  },
  useHyperLinks: {
    type: "switch",
    title: "Hide links behind the name",
    description: "Send [name](link) instead of the bare link",
    default: true,
  },
  hyperLinkText: {
    type: "text",
    title: "Link text",
    description: "{{NAME}} is replaced with the emoji or sticker name",
    default: "{{NAME}}",
  },
  renderFakeEmojis: {
    type: "switch",
    title: "Render fake emojis",
    description: "Display fake Nitro emoji links as real custom emojis",
    default: true,
  },
  renderFakeStickers: {
    type: "switch",
    title: "Render fake stickers",
    description: "Display fake Nitro sticker links as real stickers",
    default: true,
  },
  renderFakeGifs: {
    type: "switch",
    title: "Render fake GIFs",
    description: "Display hyperlinked GIFs as clean native GIF embeds",
    default: true,
  },
  renderOwnMessages: {
    type: "switch",
    title: "Include your own messages",
    description: "Also render fake emojis, stickers, and GIFs in your own messages",
    default: false,
  },
  forceFake: {
    type: "switch",
    title: "Always spoof emojis",
    description: "Send external and animated emojis as links even when you have Nitro (useful when external emojis are blocked)",
    default: false,
  },
});

interface Emoji {
  id: string;
  name: string;
  animated: boolean;
  available: boolean;
  guild: string;
}

interface CachedSticker {
  id: string;
  name: string;
  format_type: number;
  guild?: string;
}

const emojis = new Map<string, Emoji>();
const byName = new Map<string, Emoji[]>();
const stickers = new Map<string, CachedSticker>();
const channelGuild = new Map<string, string>();
// The emoji last picked for each name, which `:name:` in the message box means.
const picked = new Map<string, Emoji>();
// The emoji and sticker ids of messages we rewrote, so delayed embed-only
// MESSAGE_UPDATE frames drop their fake embeds too.
const transformed = new Map<string, Set<string>>();
const TRANSFORMED_LIMIT = 2048;
// The real tier and user id, before any spoofing, from the app's cache. Null until
// the account has synced there once.
let premiumType: number | null = null;
let currentUserId: string | null = null;

const addEmojis = (guild: string, list: Phoenix.Emoji[] | undefined) => {
  for (const e of list ?? []) {
    if (!e.id || !e.name || emojis.has(e.id)) continue;
    const emoji = { id: e.id, name: e.name, animated: !!e.animated, available: e.available !== false, guild };
    emojis.set(e.id, emoji);
    byName.set(e.name, [...(byName.get(e.name) ?? []), emoji]);
  }
};

const addStickers = (list: Phoenix.Sticker[] | undefined) => {
  for (const s of list ?? []) {
    if (!s.id || !s.name) continue;
    stickers.set(s.id, { id: s.id, name: s.name, format_type: s.format_type, guild: s.guild_id });
  }
};

// Phoenix loads guilds lazily and never hands READY to plugins: guilds and their
// emojis/stickers live in its cache. Ids in keys are zero-padded to 20 digits.
const unpad = (id: string) => id.replace(/^0+/, "");
// Desktop writes servers to its cache for minutes after READY_SUPPLEMENTAL
// (26 of 100 at first on build 2233), so reads repeat for the 5 minutes after
// it, and after that while each read finds more servers.
const SETTLE_MS = 300_000;
let settleBy = Date.now() + SETTLE_MS;
let loadedGuilds = -1;
let loadedAt = 0;
let growing = true;
const load = async () => {
  let guildCount = 0;
  try {
    const guilds = await phoenix.stores.sql<{ g: string; e: string | null; s: string | null }>(
      "SELECT k AS g, json_extract(CAST(v AS TEXT), '$.emojis') AS e," +
        " json_extract(CAST(v AS TEXT), '$.stickers') AS s FROM guilds",
    );
    guildCount = guilds.length;
    for (const { g, e, s } of guilds) {
      if (e) addEmojis(unpad(g), JSON.parse(e));
      if (s) addStickers(JSON.parse(s));
    }
  } catch {
    // Older Android SQLite builds lack json_extract, so parse the rows in JS.
    const guilds = await phoenix.stores
      .sql<{ g: string; v: string | null }>("SELECT k AS g, CAST(v AS TEXT) AS v FROM guilds")
      .catch(() => []);
    guildCount = guilds.length;
    for (const { g, v } of guilds) {
      if (!v) continue;
      try {
        const parsed = JSON.parse(v) as { emojis?: Phoenix.Emoji[]; stickers?: Phoenix.Sticker[] };
        addEmojis(unpad(g), parsed.emojis);
        addStickers(parsed.stickers);
      } catch {}
    }
  }
  growing = guildCount > loadedGuilds;
  loadedGuilds = guildCount;
  // `guild:channel` keys: Android keeps them in `channel_stubs`, Desktop in
  // `channels`. One row per guild keeps this under the 10000-row limit.
  for (const table of ["channel_stubs", "channels"]) {
    const channels = await phoenix.stores
      .sql<{ g: string; c: string }>(
        `SELECT substr(k, 1, 20) AS g, group_concat(substr(k, 22)) AS c FROM ${table}` +
          " WHERE k NOT LIKE '00000000000000000000:%' GROUP BY 1",
      )
      .catch(() => []);
    for (const { g, c } of channels) for (const id of c.split(",")) channelGuild.set(unpad(id), unpad(g));
  }
  if (growing) phoenix.log.info(
    `${emojis.size} emojis, ${stickers.size} stickers in ${guildCount} servers, ${channelGuild.size} channels`,
  );
};
const reload = () => {
  loadedAt = Date.now();
  return load().catch((err) => phoenix.log.warn(`reading the cache: ${err}`));
};
const refresh = () => {
  const now = Date.now();
  if ((growing || now < settleBy) && now - loadedAt > 15_000) reload();
};
reload();

if (gateway.guildCreate) {
  phoenix.on(gateway.guildCreate, (guild) => {
    addEmojis(guild.id, guild.emojis);
    addStickers((guild as Phoenix.GatewayGuild & { stickers?: Phoenix.Sticker[] }).stickers);
    for (const channel of guild.channels ?? []) channelGuild.set(channel.id, guild.id);
  });
}
if (gateway.channelCreate) {
  phoenix.on(gateway.channelCreate, (channel) => {
    const c = channel as Phoenix.Channel;
    if (c?.id && c.guild_id) channelGuild.set(c.id, c.guild_id);
  });
}
if (gateway.guildEmojisUpdate) {
  phoenix.on(gateway.guildEmojisUpdate, (update) => addEmojis(update.guild_id, update.emojis));
}
if (gateway.guildStickersUpdate) {
  phoenix.on(gateway.guildStickersUpdate, (update) => {
    const payload = update as { stickers?: Phoenix.Sticker[] };
    addStickers(payload.stickers);
  });
}

const applyUser = (user: { id?: string; premium_type?: number } | null | undefined) => {
  if (!user?.id) return;
  premiumType = user.premium_type ?? 0;
  // Desktop switches accounts without reloading plugins.
  if (currentUserId !== null && currentUserId !== user.id) {
    for (const map of [emojis, byName, stickers, channelGuild, picked] as Map<string, unknown>[]) map.clear();
    loadedGuilds = -1;
    settleBy = Date.now() + SETTLE_MS;
    reload();
  }
  currentUserId = user.id;
};

let fetchingMe = false;
const fetchMeFallback = () => {
  if (premiumType !== null || fetchingMe) return;
  fetchingMe = true;
  phoenix.discord
    .get("/users/@me")
    .then((res) => {
      if (res.ok) applyUser(res.json<Phoenix.CurrentUser>());
      else fetchingMe = false;
    })
    .catch(() => {
      fetchingMe = false;
    });
};

const readTier = () =>
  phoenix.discord
    .me()
    .then((user) => {
      if (user) applyUser(user);
      else fetchMeFallback();
    })
    .catch((err) => {
      phoenix.log.warn(`reading the account: ${err}`);
      fetchMeFallback();
    });
readTier();
// READY skips the event registry on mobile, so this only fires on Desktop.
if (gateway.ready) {
  phoenix.on(gateway.ready, (ready) => applyUser(ready.user));
}
if (gateway.userUpdate) {
  phoenix.on(gateway.userUpdate, (user) => applyUser(user));
}
// A first login fills the cache by now.
// ponytail: a tier bought or lost mid-session shows on the next sync.
if (gateway.readySupplemental) {
  phoenix.on(gateway.readySupplemental, () => {
    settleBy = Date.now() + SETTLE_MS;
    readTier();
    reload();
  });
}

// What Discord lets a non-Nitro user send: the server's own static emojis.
// With Nitro, anything (external emoji permission is not checked). While the
// tier is unknown, emojis go out untouched rather than as links for Nitro users.
const usable = (emoji: Emoji, guild: string | undefined) =>
  emoji.available &&
  ((!settings.forceFake && (premiumType ?? 1) > 0) || (!emoji.animated && emoji.guild === guild));

const link = (emoji: Emoji) => {
  const size = settings.emojiSize;
  const name = encodeURIComponent(emoji.name);
  const url = `https://cdn.discordapp.com/emojis/${emoji.id}.${emoji.animated ? "gif" : "webp"}?size=${size}&name=${name}&lossless=true`;
  if (!settings.useHyperLinks) return url;
  return `[${settings.hyperLinkText.replaceAll("{{NAME}}", emoji.name)}](${url})`;
};

// Rewrites `<a:name:id>` tokens and `:name:` shortcodes the sender can't use
// into links. A shortcode with a usable emoji stays: Phoenix turns a picked
// emoji's `:name:` into its token itself after onSend, by name, and would land
// inside a token put there. Links need whitespace around them to embed, one
// space between adjacent ones.
// ponytail: a `:name:` shared by several servers picks the first one we saw
// when none of them is usable here.
const rewrite = (text: string, guild: string | undefined) => {
  let linked = -1;
  return text.replace(/(?<!\\)<a?:(\w+):(\d+)>|(?<![\\<]):(\w+):(?!\d)/g, (match, _n, id, name, offset: number, whole: string) => {
    let emoji: Emoji | undefined;
    if (id) {
      emoji = emojis.get(id);
      if (!emoji || usable(emoji, guild)) return match;
    } else {
      const found = picked.has(name) ? [picked.get(name)!] : (byName.get(name) ?? []);
      if (found.some((e) => usable(e, guild))) return match;
      emoji = found.find((e) => e.available);
      if (!emoji) return match;
    }
    const end = offset + match.length;
    const before = offset > 0 && offset !== linked && !/\s/.test(whole[offset - 1]) ? " " : "";
    const after = end < whole.length && !/\s/.test(whole[end]) ? " " : "";
    linked = end;
    return before + link(emoji) + after;
  });
};

const guildOf = (ctx: Phoenix.UiContext) => (ctx.channelId ? channelGuild.get(ctx.channelId) : undefined);

// Phoenix answers a press on a locked emoji with a Nitro ad. Put `:name:` in
// the message box instead, like an unlocked pick, and remember which emoji it
// was, since several servers can have one by that name.
phoenix.messages.onEmojiPick((id, ctx) => {
  const emoji = emojis.get(id);
  if (!emoji || usable(emoji, guildOf(ctx))) return;
  picked.set(emoji.name, emoji);
  return `:${emoji.name}: `;
});

// ponytail: the tier and account are re-read for the next send, so the first
// send after an account switch still uses the last one's.
phoenix.messages.onSend((text, ctx) => {
  readTier();
  refresh();
  if (!text.includes(":")) return;
  const result = rewrite(text, guildOf(ctx));
  return result === text ? undefined : result;
});

const sendChannelId = (path: string) => {
  const clean = path.split(/[?#]/, 1)[0];
  const match = /^\/channels\/(\d+)\/messages(?:\/\d+)?$/.exec(clean);
  return match ? match[1] : undefined;
};

const stickerUsable = (sticker: CachedSticker, guild: string | undefined) =>
  (!settings.forceFake && (premiumType ?? 1) > 0) || (!!sticker.guild && sticker.guild === guild);

const stickerLink = (sticker: CachedSticker) => {
  const size = settings.stickerSize;
  const ext = sticker.format_type === 4 ? "gif" : "png";
  const name = encodeURIComponent(sticker.name);
  const animated = sticker.format_type === 2 || sticker.format_type === 4 ? "&animated=true" : "";
  const url = `https://media.discordapp.net/stickers/${sticker.id}.${ext}?size=${size}&name=${name}${animated}`;
  if (!settings.useHyperLinks) return url;
  return `[${settings.hyperLinkText.replaceAll("{{NAME}}", sticker.name)}](${url})`;
};

type SendBody = { content?: unknown; sticker_ids?: unknown; [key: string]: unknown };

// Moves the stickers the sender cannot post natively out of `sticker_ids` and
// into the content as CDN links. Returns whether it changed the body.
const linkStickers = (body: SendBody, guild: string | undefined) => {
  if (!settings.enableStickerBypass || !Array.isArray(body.sticker_ids) || !body.sticker_ids.length) {
    return false;
  }
  const links: string[] = [];
  const kept: string[] = [];
  for (const id of body.sticker_ids as string[]) {
    const sticker = stickers.get(id);
    if (!sticker || stickerUsable(sticker, guild)) kept.push(id);
    else links.push(stickerLink(sticker));
  }
  if (!links.length) return false;
  if (kept.length) body.sticker_ids = kept;
  else delete body.sticker_ids;
  const existing = typeof body.content === "string" ? body.content.trim() : "";
  body.content = existing ? `${existing} ${links.join(" ")}` : links.join(" ");
  return true;
};

// Sends and edits the composer hook missed, like a picked emoji the mirror
// didn't see, and stickers. Text onSend already rewrote comes through unchanged.
const rewriteRequest = (req: Phoenix.InterceptedRequest): Phoenix.RequestHookResult => {
  if (!req.body.includes(":") && !req.body.includes("sticker_ids")) return;
  const parsed = req.json<SendBody>();
  if (!parsed) return;
  const body = { ...parsed };
  const channelId = sendChannelId(req.path);
  const guild = channelId ? channelGuild.get(channelId) : undefined;
  let changed = linkStickers(body, guild);
  if (typeof body.content === "string" && body.content.includes(":")) {
    const result = rewrite(body.content, guild);
    if (result !== body.content) {
      body.content = result;
      changed = true;
    }
  }
  return changed ? { body } : undefined;
};

phoenix.discord.onRequest({ method: "POST", pathPrefix: "/channels/", pathSuffix: "/messages" }, rewriteRequest);
phoenix.discord.onRequest({ method: "PATCH", pathPrefix: "/channels/", pathIncludes: "/messages/" }, rewriteRequest);

// --- Nitro color themes: keep the picked gradient on this client ---
//
// Appearance sends the gradient as `client_theme_settings` (field 3) inside
// the settings proto's appearance message (field 13), and Discord's servers
// drop it for accounts without Nitro: the PATCH response, and READY's
// `user_settings_proto` after a restart, come back with only the gradient's
// base theme. The last gradient sent is kept in storage and put back into
// both.

const APPEARANCE_FIELD = 13;
const CLIENT_THEME_FIELD = 3;
const THEME_KEY = "clientThemeSettings";

// Plugins type-check against ES2023 alone, without the DOM's atob and btoa.
const BASE64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

const fromBase64 = (text: string) => {
  const clean = text.replace(/[^A-Za-z0-9+/]/g, "");
  const out = new Uint8Array(Math.floor((clean.length * 3) / 4));
  let bits = 0;
  let value = 0;
  let at = 0;
  for (const char of clean) {
    value = ((value << 6) | BASE64.indexOf(char)) & 0xffffff;
    bits += 6;
    if (bits >= 8) {
      bits -= 8;
      out[at++] = (value >> bits) & 0xff;
    }
  }
  return out.subarray(0, at);
};

const toBase64 = (bytes: Uint8Array) => {
  let text = "";
  for (let i = 0; i < bytes.length; i += 3) {
    const chunk = (bytes[i] << 16) | ((bytes[i + 1] ?? 0) << 8) | (bytes[i + 2] ?? 0);
    text += BASE64[(chunk >> 18) & 63] + BASE64[(chunk >> 12) & 63];
    text += i + 1 < bytes.length ? BASE64[(chunk >> 6) & 63] : "=";
    text += i + 2 < bytes.length ? BASE64[chunk & 63] : "=";
  }
  return text;
};

interface ProtoField {
  field: number;
  wire: number;
  // The whole field, tag included, and a length-delimited field's payload.
  start: number;
  end: number;
  payload: number;
}

const readVarint = (bytes: Uint8Array, at: number): [number, number] => {
  let value = 0;
  let shift = 0;
  while (at < bytes.length) {
    const byte = bytes[at++];
    value += (byte & 0x7f) * 2 ** shift;
    if (!(byte & 0x80)) return [value, at];
    shift += 7;
  }
  throw new Error("truncated varint");
};

const writeVarint = (value: number) => {
  const out: number[] = [];
  while (value > 0x7f) {
    out.push((value % 0x80) | 0x80);
    value = Math.floor(value / 0x80);
  }
  out.push(value);
  return out;
};

const parseProto = (bytes: Uint8Array): ProtoField[] => {
  const fields: ProtoField[] = [];
  let at = 0;
  while (at < bytes.length) {
    const start = at;
    const [tag, afterTag] = readVarint(bytes, at);
    const wire = tag % 8;
    let payload = afterTag;
    if (wire === 0) [, at] = readVarint(bytes, afterTag);
    else if (wire === 1) at = afterTag + 8;
    else if (wire === 5) at = afterTag + 4;
    else if (wire === 2) {
      const [len, afterLen] = readVarint(bytes, afterTag);
      payload = afterLen;
      at = afterLen + len;
    } else throw new Error(`unsupported wire type ${wire}`);
    if (at > bytes.length) throw new Error("truncated field");
    fields.push({ field: Math.floor(tag / 8), wire, start, end: at, payload });
  }
  return fields;
};

const lengthDelimited = (field: number, payload: Uint8Array) =>
  Uint8Array.from([...writeVarint(field * 8 + 2), ...writeVarint(payload.length), ...payload]);

const concat = (parts: Uint8Array[]) => {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let at = 0;
  for (const part of parts) {
    out.set(part, at);
    at += part.length;
  }
  return out;
};

// The appearance message of a settings proto, or undefined without one.
const appearanceOf = (proto: Uint8Array) => {
  const field = parseProto(proto).find((f) => f.field === APPEARANCE_FIELD && f.wire === 2);
  return field ? proto.subarray(field.payload, field.end) : undefined;
};

// Remembers the gradient a sent settings proto picks, or forgets it when the
// proto picks a theme without one.
const rememberTheme = (settingsBase64: string) => {
  const appearance = appearanceOf(fromBase64(settingsBase64));
  if (!appearance) return;
  const fields = parseProto(appearance);
  const theme = fields.find((f) => f.field === CLIENT_THEME_FIELD && f.wire === 2);
  // A default theme is sent with an empty `client_theme_settings`.
  const gradient = theme && appearance.slice(theme.payload, theme.end);
  if (gradient && gradient.length) {
    phoenix.storage.set(THEME_KEY, toBase64(gradient));
  } else if (theme || fields.some((f) => f.field === 1)) {
    phoenix.storage.delete(THEME_KEY);
  }
};

// Puts the kept gradient back into a settings proto that came back without
// one. Returns the new base64, or undefined when nothing changes.
const restoreTheme = (settingsBase64: string) => {
  const saved = phoenix.storage.get<string>(THEME_KEY);
  if (!saved) return;
  const proto = fromBase64(settingsBase64);
  const top = parseProto(proto);
  const field = top.find((f) => f.field === APPEARANCE_FIELD && f.wire === 2);
  const appearance = field ? proto.subarray(field.payload, field.end) : new Uint8Array();
  const inner = parseProto(appearance);
  const theme = inner.find((f) => f.field === CLIENT_THEME_FIELD);
  if (theme && theme.end > theme.payload) return;
  // An empty `client_theme_settings` is dropped and replaced with the kept one.
  const kept = theme
    ? concat([appearance.subarray(0, theme.start), appearance.subarray(theme.end)])
    : appearance;
  const patched = lengthDelimited(
    APPEARANCE_FIELD,
    concat([kept, lengthDelimited(CLIENT_THEME_FIELD, fromBase64(saved))]),
  );
  const parts = field
    ? [proto.subarray(0, field.start), patched, proto.subarray(field.end)]
    : [proto, patched];
  return toBase64(concat(parts));
};

const SETTINGS_PROTO = { pathPrefix: "/users/@me/settings-proto/1" };

phoenix.discord.onRequest({ method: "PATCH", ...SETTINGS_PROTO }, (req) => {
  if (!settings.enableAppearanceBypass) return;
  try {
    const body = req.json<{ settings?: string }>();
    if (body?.settings) rememberTheme(body.settings);
  } catch (err) {
    phoenix.log.warn(`reading the sent theme: ${err}`);
  }
});

const restoreResponse = (res: Phoenix.InterceptedResponse) => {
  if (!settings.enableAppearanceBypass || res.status < 200 || res.status >= 300) return;
  try {
    const body = res.json<{ settings?: string }>();
    const restored = body?.settings && restoreTheme(body.settings);
    return restored ? { body: { ...body, settings: restored } } : undefined;
  } catch (err) {
    phoenix.log.warn(`restoring the theme: ${err}`);
  }
};
phoenix.discord.onResponse({ method: "PATCH", ...SETTINGS_PROTO }, restoreResponse);
phoenix.discord.onResponse({ method: "GET", ...SETTINGS_PROTO }, restoreResponse);

// --- Perks fallback: where this build has no native unlock (Linux, macOS, or a
// Windows build a locator missed), grant the perks the client reads itself ---

const PERK_STREAM_MID_QUALITY = 9;
const PERK_STREAM_HIGH_QUALITY = 10;
const PERK_CLIENT_THEMES = 17;
const PERK_APP_ICONS = 27;

type Perks = { active_perks_bitmask?: unknown; config_by_perk?: Record<string, unknown> | null };

const fallbackPerks = () => {
  const perks: number[] = [];
  if (settings.enableStreamQualityBypass && !phoenix.stream?.native) {
    perks.push(PERK_STREAM_MID_QUALITY, PERK_STREAM_HIGH_QUALITY);
  }
  if (settings.enableAppearanceBypass && !phoenix.appearance?.native) {
    perks.push(PERK_CLIENT_THEMES, PERK_APP_ICONS);
  }
  return perks;
};

// Each word holds 64 perks: `|` keeps only 32 bits, and a double holds the rest
// exactly only up to 2^53, so bits are added and an unsafe word is left alone.
const grantPerks = (user: unknown): boolean => {
  const perks = (user as { perks?: Perks } | null | undefined)?.perks;
  const words = perks?.active_perks_bitmask;
  if (!perks || !Array.isArray(words)) return false;
  let changed = false;
  for (const perk of fallbackPerks()) {
    const at = Math.floor(perk / 64);
    while (words.length <= at) words.push(0);
    const word: unknown = words[at];
    const value = 2 ** (perk % 64);
    if (typeof word !== "number" || !Number.isSafeInteger(word)) continue;
    if (Math.floor(word / value) % 2 === 0) {
      if (!Number.isSafeInteger(word + value)) continue;
      words[at] = word + value;
      changed = true;
    }
    // A promotion grants a perk limited to a few options, like one theme or
    // one icon, which would keep the others locked.
    if (perks.config_by_perk && String(perk) in perks.config_by_perk) {
      delete perks.config_by_perk[String(perk)];
      changed = true;
    }
  }
  return changed;
};

if (fallbackPerks().length) {
  phoenix.log.info(`no native unlock for perks ${fallbackPerks().join(", ")}, granting them client-side`);
}

if (gateway.ready) {
  phoenix.hook(gateway.ready, {
    before(ready) {
      const payload = ready as { user?: unknown; user_settings_proto?: string };
      grantPerks(payload.user);
      if (!settings.enableAppearanceBypass || !payload.user_settings_proto) return;
      try {
        const restored = restoreTheme(payload.user_settings_proto);
        if (restored) payload.user_settings_proto = restored;
      } catch (err) {
        phoenix.log.warn(`restoring the theme at READY: ${err}`);
      }
    },
  });
}

if (gateway.userUpdate) {
  phoenix.hook(gateway.userUpdate, {
    before(user) {
      grantPerks(user);
    },
  });
}

phoenix.discord.onResponse({ method: "GET", pathPrefix: "/users/@me" }, (res) => {
  if (res.path.split("?")[0] !== "/users/@me" || res.status !== 200) return;
  const user = res.json<object>();
  return user && grantPerks(user) ? { body: user } : undefined;
});

syncStreamUnlock(settings.enableStreamQualityBypass);
syncAppearanceUnlock(settings.enableAppearanceBypass);
phoenix.onUnload(() => {
  syncStreamUnlock(false);
  syncAppearanceUnlock(false);
});

// --- Incoming rendering: turn fake Nitro emojis, stickers, and GIFs into real ones ---

const CODE_SPLIT = /(```[\s\S]*?```|`[^`\n]+`)/g;
// `[label](url)` or a bare url (not inside `<...>`, `(...)` or `[...]`) to a CDN asset.
const cdnLinks = (path: string, exts: string) => {
  const url = `https?:\\/\\/(?:cdn\\.discordapp\\.com|media\\.discordapp\\.net)\\/${path}\\/\\d{15,22}\\.(?:${exts})(?:[?#][^\\s)<>]*)?`;
  return new RegExp(`\\[([^\\]\\n]*)\\]\\((${url})\\)|(?<![<(\\[])(${url})`, "gi");
};
const EMOJI_LINK_RE = cdnLinks("emojis", "png|jpe?g|webp|gif");
const STICKER_LINK_RE = cdnLinks("stickers", "png|webp|gif|apng|json");
// The id and extension of a CDN asset url, or of an embed's.
const ASSET_RE = /\/(emojis|stickers)\/(\d{15,22})\.(\w+)/i;
const GIF_HYPERLINK_RE = /\[([^\]\n]*)\]\((https?:\/\/[^\s)<>]+)\)/g;
const GIF_URL_RE =
  /^https?:\/\/(?:(?:www\.)?(?:tenor\.com\/view\/|giphy\.com\/gifs\/|klipy\.com\/gifs\/)|(?:media\d*|c|i)\.tenor\.com\/|(?:media\d*|i)\.giphy\.com\/|static\.klipy\.com\/|[^\s?#]+\.gif(?:[?#]|$))/i;
const INVISIBLE_LABEL_RE = /^[\s​-‏⁠⠀﻿._-]*$/;

const safeDecode = (s: string) => {
  try {
    return decodeURIComponent(s.replaceAll("+", " "));
  } catch {
    return s;
  }
};

// QuickJS has no URLSearchParams.
const parseQuery = (url: string) => {
  const out = new Map<string, string>();
  const q = url.indexOf("?");
  if (q === -1) return out;
  const hash = url.indexOf("#", q);
  for (const part of url.slice(q + 1, hash === -1 ? url.length : hash).split("&")) {
    if (!part) continue;
    const eq = part.indexOf("=");
    const key = eq === -1 ? part : part.slice(0, eq);
    out.set(safeDecode(key).toLowerCase(), eq === -1 ? "" : safeDecode(part.slice(eq + 1)));
  }
  return out;
};

const emojiName = (raw: string | undefined) => {
  const cleaned = (raw ?? "").replace(/^:+|:+$/g, "").replace(/[^a-zA-Z0-9_]/g, "");
  return cleaned.length >= 2 ? cleaned.slice(0, 32) : "";
};

const stickerName = (raw: string | undefined) =>
  (raw ?? "").replace(/[​-‏⁠⠀﻿]/g, "").trim().slice(0, 32);

// Sticker format types: 1 PNG, 2 APNG, 3 Lottie, 4 GIF.
const stickerFormat = (id: string, ext: string, params: Map<string, string>) => {
  const cached = stickers.get(id);
  if (cached) return cached.format_type;
  if (ext === "gif") return 4;
  if (ext === "json") return 3;
  if (ext === "apng" || params.get("animated")?.toLowerCase() === "true") return 2;
  return 1;
};

const rememberTransformed = (id: string, ids: Set<string>) => {
  transformed.set(id, ids);
  if (transformed.size > TRANSFORMED_LIMIT) {
    const oldest = transformed.keys().next().value;
    if (oldest !== undefined) transformed.delete(oldest);
  }
};

// Drops the image embeds Discord made for links we turned into emojis or stickers.
const stripFakeEmbeds = (embeds: Phoenix.Embed[] | undefined, ids: Set<string>) => {
  if (!embeds?.length || !ids.size) return false;
  const kept = embeds.filter(
    (embed) =>
      ![embed.url, embed.image?.url, embed.thumbnail?.url].some((url) => {
        const asset = url && ASSET_RE.exec(url);
        return !!asset && ids.has(asset[2]);
      }),
  );
  if (kept.length === embeds.length) return false;
  embeds.splice(0, embeds.length, ...kept);
  return true;
};

type TransformableMessage = {
  id?: string;
  author?: { id?: string };
  content?: string;
  embeds?: Phoenix.Embed[];
  sticker_items?: Phoenix.StickerItem[];
  referenced_message?: TransformableMessage | null;
  message_snapshots?: { message?: TransformableMessage }[];
};

const rendering = () => settings.renderFakeEmojis || settings.renderFakeStickers || settings.renderFakeGifs;

// Rewrites fake emoji and sticker links outside code into real ones, and
// unwraps hyperlinked GIFs. Returns whether the text changed.
const renderContent = (content: string, ids: Set<string>, found: Phoenix.StickerItem[]) => {
  const parts = content.split(CODE_SPLIT);
  for (let i = 0; i < parts.length; i += 2) {
    if (!parts[i].includes("http")) continue;
    if (settings.renderFakeEmojis) {
      parts[i] = parts[i].replace(EMOJI_LINK_RE, (_m, label, mdUrl, bareUrl) => {
        const url: string = mdUrl ?? bareUrl;
        const [, , id, ext] = ASSET_RE.exec(url)!;
        const params = parseQuery(url);
        const flag = params.get("animated")?.toLowerCase();
        const animated =
          ext.toLowerCase() === "gif" || flag === "true" || flag === "1" || (flag !== "false" && !!emojis.get(id)?.animated);
        ids.add(id);
        const name = emojiName(params.get("name")) || emojis.get(id)?.name || emojiName(label) || "emoji";
        return `<${animated ? "a" : ""}:${name}:${id}>`;
      });
    }
    if (settings.renderFakeStickers) {
      parts[i] = parts[i].replace(STICKER_LINK_RE, (_m, label, mdUrl, bareUrl) => {
        const url: string = mdUrl ?? bareUrl;
        const [, , id, ext] = ASSET_RE.exec(url)!;
        const params = parseQuery(url);
        ids.add(id);
        if (!found.some((s) => s.id === id)) {
          const name = stickers.get(id)?.name || stickerName(params.get("name")) || stickerName(label) || "Sticker";
          found.push({ id, name, format_type: stickerFormat(id, ext.toLowerCase(), params) });
        }
        return "";
      });
    }
  }
  let text = parts.join("");
  if (found.length) text = text.replace(/[ \t]{2,}/g, " ").trim();

  if (settings.renderFakeGifs && text.includes("](")) {
    // A message that is only a hyperlinked GIF shows as the GIF alone when the link is bare.
    const alone = /^\[[^\]\n]*\]\((https?:\/\/[^\s)<>]+)\)$/.exec(text.trim());
    if (alone && GIF_URL_RE.test(alone[1])) return alone[1];
    // Invisible-label GIF links beside text only hide the GIF's url; its embed stays.
    const gifParts = text.split(CODE_SPLIT);
    for (let i = 0; i < gifParts.length; i += 2) {
      gifParts[i] = gifParts[i].replace(GIF_HYPERLINK_RE, (match, label: string, url: string) =>
        GIF_URL_RE.test(url) && INVISIBLE_LABEL_RE.test(label) ? "" : match,
      );
    }
    const unwrapped = gifParts.join("");
    if (unwrapped !== text) text = unwrapped.replace(/[ \t]{2,}/g, " ").trim();
  }
  return text;
};

const transformSingle = (message: TransformableMessage): boolean => {
  if (!rendering()) return false;
  if (!settings.renderOwnMessages && currentUserId && message.author?.id === currentUserId) return false;

  // Embed-only MESSAGE_UPDATE frames carry no `content`; strip fake embeds we already converted.
  if (typeof message.content !== "string") {
    const prev = message.id && transformed.get(message.id);
    return !!prev && stripFakeEmbeds(message.embeds, prev);
  }
  if (!message.content.includes("http")) return false;

  const ids = new Set<string>();
  const found: Phoenix.StickerItem[] = [];
  const content = renderContent(message.content, ids, found);
  let changed = content !== message.content;
  message.content = content;

  const existing = message.sticker_items ?? [];
  const added = found.filter((s) => !existing.some((e) => e.id === s.id));
  if (added.length) {
    message.sticker_items = [...existing, ...added];
    changed = true;
  }
  if (ids.size) {
    if (message.id) rememberTransformed(message.id, ids);
    if (stripFakeEmbeds(message.embeds, ids)) changed = true;
  }
  return changed;
};

const transformMessage = (message: TransformableMessage | null | undefined): boolean => {
  if (!message || typeof message !== "object") return false;
  let changed = transformSingle(message);
  if (transformMessage(message.referenced_message)) changed = true;
  for (const snap of message.message_snapshots ?? []) {
    if (transformMessage(snap?.message)) changed = true;
  }
  return changed;
};

for (const event of [gateway.messageCreate, gateway.messageUpdate]) {
  phoenix.hook(event, {
    before(message) {
      refresh();
      transformMessage(message);
    },
  });
}

// History loads pass through here on every channel open, so pages with
// nothing to render are left unparsed.
const worthParsing = (res: Phoenix.InterceptedResponse) =>
  res.status >= 200 &&
  res.status < 300 &&
  rendering() &&
  (res.body.includes("/emojis/") || res.body.includes("/stickers/") || res.body.includes("]("));

// A single message (POST/PATCH), a message list, the pins' `{ items: [{ message }] }`,
// or search's `{ messages: [[message]] }`.
const transformResponse = (res: Phoenix.InterceptedResponse) => {
  if (!worthParsing(res)) return;
  const data = res.json<
    | TransformableMessage
    | TransformableMessage[]
    | { items?: { message?: TransformableMessage }[]; messages?: TransformableMessage[][] }
  >();
  if (!data) return;
  const list: (TransformableMessage | undefined)[] = Array.isArray(data)
    ? data
    : "items" in data || "messages" in data
      ? [...(data.items ?? []).map((entry) => entry?.message), ...(data.messages ?? []).flat()]
      : [data as TransformableMessage];
  let changed = false;
  for (const message of list) {
    if (transformMessage(message)) changed = true;
  }
  return changed ? { body: data } : undefined;
};

for (const [method, pathPrefix, pathSuffix] of [
  ["GET", "/channels/", "/messages"],
  ["POST", "/channels/", "/messages"],
  ["GET", "/channels/", "/pins"],
  ["GET", "/channels/", "/messages/search"],
  ["GET", "/guilds/", "/messages/search"],
] as const) {
  phoenix.discord.onResponse({ method, pathPrefix, pathSuffix }, transformResponse);
}
phoenix.discord.onResponse(
  { method: "PATCH", pathPrefix: "/channels/", pathIncludes: "/messages/" },
  transformResponse,
);
