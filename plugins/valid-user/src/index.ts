// @description Fixes mentions of people Discord hasn't loaded showing up as @unknown-user
// @version 1.0.0
// @author yuax

type Msg = Partial<Phoenix.GatewayMessage> & { id: string; channel_id: string };

const MENTION = /<@!?(\d{17,20})>/g;
// Discord rate limits profile lookups, so they go out a couple at a time.
const PARALLEL = 2;

const users = new Map<string, Phoenix.User | null>();
const waiting = new Map<string, Map<string, Msg>>();
const resolvers = new Map<string, (() => void)[]>();
const queue: string[] = [];
let running = 0;
let pausedUntil = 0;

const { gateway } = phoenix;

function texts(msg: Partial<Phoenix.Message>): string[] {
  const snapshots = (msg.message_snapshots ?? []).map((s) => s.message);
  const out = [msg.content ?? "", ...snapshots.map((s) => s.content)];
  for (const e of [...(msg.embeds ?? []), ...snapshots.flatMap((s) => s.embeds ?? [])]) {
    out.push(e.title ?? "", e.description ?? "", e.footer?.text ?? "", e.author?.name ?? "");
    for (const f of e.fields ?? []) out.push(f.name, f.value);
  }
  return out;
}

/// Adds the users already looked up to the message, and returns the mentioned ids still unknown.
function fill(msg: Partial<Phoenix.Message>): string[] {
  const have = new Set((msg.mentions ?? []).map((u) => u.id));
  const unknown = new Set<string>();
  for (const text of texts(msg)) {
    for (const [, id] of text.matchAll(MENTION)) {
      if (have.has(id)) continue;
      const user = users.get(id);
      if (user) {
        msg.mentions = [...(msg.mentions ?? []), user];
        have.add(id);
      } else if (user === undefined) {
        unknown.add(id);
      }
    }
  }
  return [...unknown];
}

function lookup(id: string): Promise<void> {
  if (users.has(id)) return Promise.resolve();
  if (!queue.includes(id) && !resolvers.has(id)) queue.push(id);
  const done = new Promise<void>((resolve) => resolvers.set(id, [...(resolvers.get(id) ?? []), resolve]));
  void pump();
  return done;
}

async function pump() {
  // There are no timers: after a 429 the queue picks up again on the next message.
  while (running < PARALLEL && queue.length && Date.now() >= pausedUntil) {
    const id = queue.shift()!;
    running++;
    try {
      // `/users/{id}` is for bots and answers user accounts with 403, the profile works for them.
      const res = await phoenix.discord.get(`/users/${id}/profile?with_mutual_guilds=false&with_mutual_friends=false`);
      if (res.status === 429) {
        pausedUntil = Date.now() + Number(res.json<{ retry_after?: number }>().retry_after ?? 1) * 1000;
        queue.unshift(id);
        continue;
      }
      users.set(id, (res.ok && res.json<{ user?: Phoenix.User }>().user) || null);
    } catch {
      users.set(id, null);
    } finally {
      running--;
    }
    for (const resolve of resolvers.get(id) ?? []) resolve();
    resolvers.delete(id);
    repaint(id);
  }
}

/// Hands Phoenix the messages that were waiting on `id`, now with the user.
function repaint(id: string) {
  const pending = waiting.get(id);
  waiting.delete(id);
  for (const msg of pending?.values() ?? []) {
    const before = msg.mentions?.length ?? 0;
    fill(msg);
    if ((msg.mentions?.length ?? 0) === before) continue;
    phoenix.enqueue(gateway.messageUpdate, {
      id: msg.id,
      channel_id: msg.channel_id,
      ...(msg.guild_id ? { guild_id: msg.guild_id } : {}),
      mentions: msg.mentions,
    });
  }
}

function watch(message: Phoenix.GatewayMessage) {
  if (!message.id || !message.channel_id) return;
  const copy: Msg = {
    id: message.id,
    channel_id: message.channel_id,
    guild_id: message.guild_id,
    content: message.content,
    embeds: message.embeds,
    message_snapshots: message.message_snapshots,
    mentions: message.mentions,
  };
  for (const id of fill(message)) {
    waiting.set(id, (waiting.get(id) ?? new Map()).set(message.id, copy));
    void lookup(id);
  }
}

phoenix.hook(gateway.messageCreate, { before: watch });
phoenix.hook(gateway.messageUpdate, { before: watch });

// Channel history waits for the lookups, so the messages arrive whole. When that takes longer than
// the runtime lets a reply wait, Phoenix gets the reply as it was and the updates follow.
phoenix.discord.onResponse({ method: "GET", pathPrefix: "/channels/", pathSuffix: "/messages" }, async (res) => {
  if (res.status < 200 || res.status >= 300) return;
  const list = res.json<Phoenix.GatewayMessage[]>();
  if (!Array.isArray(list)) return;
  const ids = [...new Set(list.flatMap((m) => fill(m)))];
  if (!ids.length) return;
  for (const m of list) watch(m);
  await Promise.all(ids.map(lookup));
  for (const m of list) fill(m);
  return { body: list };
});
