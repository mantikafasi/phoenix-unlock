const { gateway } = phoenix;

const settings = phoenix.settings.define({
   findInServer: {
      type: "switch",
      title: "Look in the server first",
      description: "Use an emoji of the same name from the message's server before the ID below, useful without Nitro",
      default: true,
   },
   emojiName: {
      type: "text",
      title: "Emoji name",
      description: "The emoji's name, husk from the Vencord server by default",
      default: "husk",
   },
   emojiID: {
      type: "text",
      title: "Emoji ID",
      description: "The emoji to use when none is found in the server",
      default: "1026532993923293184",
   },
});

// What the app tells us about each server, since the plugin has no emoji store to ask.
const emojisByGuild = new Map<string, Phoenix.Emoji[]>();
const guildOfChannel = new Map<string, string>();

if (gateway.guildCreate) {
   phoenix.on(gateway.guildCreate, (guild) => {
      emojisByGuild.set(guild.id, guild.emojis ?? []);
      for (const channel of guild.channels ?? []) guildOfChannel.set(channel.id, guild.id);
   });
}
if (gateway.guildEmojisUpdate) {
   phoenix.on(gateway.guildEmojisUpdate, (update) => emojisByGuild.set(update.guild_id, update.emojis));
}
phoenix.on(gateway.messageCreate, (message) => {
   if (message.guild_id) guildOfChannel.set(message.channel_id, message.guild_id);
});

function emojiIdFor(channelId: string, guildId: string | undefined): string {
   const guild = guildId ?? guildOfChannel.get(channelId);
   if (settings.findInServer && guild) {
      const found = emojisByGuild.get(guild)?.find((emoji) => emoji.name === settings.emojiName && emoji.id);
      if (found?.id) return found.id;
   }
   return settings.emojiID.trim();
}

phoenix.ui.addMessageAction({
   label: "Husk",
   icon: "res://ic_mana_emoji_face_with_crossed_out_eyes",
   async onPress(ctx) {
      // The cached message can be missing, so go by the ids every press carries.
      const channelId = ctx.channelId ?? ctx.message?.channel_id;
      if (!channelId) {
         phoenix.ui.toast("Husk: no channel to react in");
         return;
      }
      const guildId = (ctx.message as { guild_id?: string } | undefined)?.guild_id;
      const emoji = `${settings.emojiName}:${emojiIdFor(channelId, guildId)}`;
      const res = await phoenix.discord.put(
         `/channels/${channelId}/messages/${ctx.messageId}/reactions/${encodeURIComponent(emoji)}/@me`
      );
      if (!res.ok) phoenix.ui.toast(`Husk failed (${res.status})`);
   },
});
