// @description Blurs image and video previews in age-restricted channels
// @version 0.1.0
// @author wolfieeeeeeee

const { gateway } = phoenix;

const settings = phoenix.settings.define({
  allChannels: {
    type: "switch",
    title: "Blur in all channels",
    description: "Also blur image previews outside age-restricted channels",
    default: false,
  },
});

const CHANNEL_TTL_MS = 60_000;
const IMAGE_EXTENSION = /\.(?:gif|jpe?g|png|webp)$/i;
const VIDEO_EXTENSION = /\.(?:m4v|mkv|mov|mp4|webm)$/i;
const MESSAGE_PATH = /^\/channels\/(\d+)\/messages(?:\/\d+)?(?:\?|$)/;

type Channel = Pick<Phoenix.Channel, "id" | "nsfw" | "parent_id">;

const channels = new Map<string, { channel: Channel; expires: number }>();
const lookups = new Map<string, Promise<Channel | undefined>>();

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null;

const rememberChannel = (value: unknown) => {
  if (!isRecord(value) || typeof value.id !== "string" || !/^\d+$/.test(value.id)) {
    return;
  }
  const channel: Channel = {
    id: value.id,
    nsfw: value.nsfw === true,
    parent_id: typeof value.parent_id === "string" && /^\d+$/.test(value.parent_id)
      ? value.parent_id
      : null,
  };
  channels.set(channel.id, { channel, expires: Date.now() + CHANNEL_TTL_MS });
  return channel;
};

const cachedChannel = (id: string) => {
  const entry = channels.get(id);
  if (entry && entry.expires > Date.now()) return entry.channel;
};

const loadChannel = (id: string): Promise<Channel | undefined> => {
  const cached = cachedChannel(id);
  if (cached) return Promise.resolve(cached);

  const existing = lookups.get(id);
  if (existing) return existing;

  const pending = phoenix.discord.get(`/channels/${id}`)
    .then((response) => {
      if (!response.ok) return;
      const value = response.json();
      if (isRecord(value) && value.id === id) {
        return cachedChannel(id) ?? rememberChannel(value);
      }
    })
    .catch(() => undefined)
    .finally(() => lookups.delete(id));

  lookups.set(id, pending);
  return pending;
};

const isAgeRestricted = async (id: string) => {
  const channel = await loadChannel(id);
  if (channel?.nsfw) return true;
  return channel?.parent_id
    ? (await loadChannel(channel.parent_id))?.nsfw === true
    : false;
}

const visual = (filename: string, contentType?: string) =>
  /^(?:image|video)\//i.test(contentType ?? "") ||
  IMAGE_EXTENSION.test(filename) || VIDEO_EXTENSION.test(filename);

const spoilerComponents = (components: Phoenix.Component[]): boolean => {
  let changed = false;
  for (const component of components) {
    switch (component.type) {
      case 9:
        if (component.accessory.type === 11 && !component.accessory.spoiler) {
          component.accessory.spoiler = true;
          changed = true;
        }
        break;
      case 11:
        if (!component.spoiler) {
          component.spoiler = true;
          changed = true;
        }
        break;
      case 12:
        for (const item of component.items) {
          if (!item.spoiler) {
            item.spoiler = true;
            changed = true;
          }
        }
        break;
      case 17:
        if (spoilerComponents(component.components)) changed = true;
        break;
    }
  }
  return changed;
};

type MessageMedia = Pick<Phoenix.Message,
  "content" | "attachments" | "embeds" | "referenced_message" | "message_snapshots"
> & Partial<Pick<Phoenix.Message, "components">>;

type ComponentEmbed = Phoenix.Embed & { components?: Phoenix.Component[] };

const spoilerEmbed = (embed: ComponentEmbed): boolean => {
  if (embed.components) return spoilerComponents(embed.components);
  const images = [embed.image, embed.thumbnail].filter((item) => item !== undefined);
  const media = embed.video ? [embed.video] : images;
  if (!media.length) return false;

  const components: Phoenix.Component[] = [];
  const text = [
    embed.author?.name,
    embed.title,
    embed.description,
    ...(embed.fields ?? []).map((field) => `${field.name}\n${field.value}`),
  ].filter(Boolean).join("\n\n");
  if (text) components.push({ type: 10, content: text });
  components.push({
    type: 12,
    items: media.map((item) => ({
      media: { ...item },
      description: item.description,
      spoiler: true,
    })),
  });
  const footer = [embed.footer?.text, embed.provider?.name, embed.timestamp]
    .filter(Boolean).join(" · ");
  if (footer) components.push({ type: 10, content: footer });

  embed.type = "components";
  embed.components = [{ type: 17, accent_color: embed.color, components }];
  delete embed.image;
  delete embed.thumbnail;
  delete embed.video;
  return true;
};

const transformMessage = (message: MessageMedia | null | undefined): boolean => {
  if (!message) return false;
  let changed = false;
  for (const attachment of message.attachments ?? []) {
    if (!visual(attachment.filename, attachment.content_type)) continue;
    if (attachment.is_spoiler && ((attachment.flags ?? 0) & 8)) continue;
    attachment.is_spoiler = true;
    attachment.flags = (attachment.flags ?? 0) | 8;
    if (!attachment.filename.startsWith("SPOILER_")) {
      attachment.filename = `SPOILER_${attachment.filename}`;
    }
    changed = true;
  }
  for (const embed of message.embeds ?? []) {
    if (spoilerEmbed(embed)) changed = true;
  }
  if (spoilerComponents(message.components ?? [])) changed = true;
  if (transformMessage(message.referenced_message)) changed = true;
  for (const snapshot of message.message_snapshots ?? []) {
    if (transformMessage(snapshot.message)) changed = true;
  }
  return changed;
};

for (const event of [gateway.channelCreate, gateway.channelUpdate, gateway.threadCreate, gateway.threadUpdate]) {
  if (!event) continue;
  phoenix.on(event, (channel) => {
    rememberChannel(channel);
  });
}
if (gateway.guildCreate) {
  phoenix.on(gateway.guildCreate, (guild) => {
    for (const channel of guild.channels ?? []) rememberChannel(channel);
  });
}

for (const event of [gateway.messageCreate, gateway.messageUpdate]) {
  if (!event) continue;
  phoenix.hook(event, {
    before(message) {
      const id = message.channel_id;
      if (settings.allChannels) {
        transformMessage(message);
        return;
      }
      if (typeof id !== "string" || !/^\d+$/.test(id)) return;
      const channel = channels.get(id)?.channel;
      const parentId = channel?.parent_id;
      // Gateway hooks are synchronous. Use the last known state while refreshing it.
      if (!cachedChannel(id) || (parentId && !cachedChannel(parentId))) {
        void isAgeRestricted(id);
      }
      if (channel?.nsfw || (parentId && channels.get(parentId)?.channel.nsfw)) {
        transformMessage(message);
      }
    },
  });
}

const transformResponse = async (res: Phoenix.InterceptedResponse) => {
  const id = MESSAGE_PATH.exec(res.path)?.[1];
  if (!id || res.status !== 200) return;
  if (!settings.allChannels && !(await isAgeRestricted(id))) return;

  const data = res.json<MessageMedia[] | MessageMedia>();
  if (!data) return;

  let changed = false;
  for (const message of Array.isArray(data) ? data : [data]) {
    if (transformMessage(message)) changed = true;
  }
  return changed ? { body: data } : undefined;
};

phoenix.discord.onResponse(
  { method: "GET", pathPrefix: "/channels/", pathIncludes: "/messages" },
  transformResponse,
);
