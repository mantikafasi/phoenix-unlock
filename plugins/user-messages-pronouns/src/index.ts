const AUTO_MODERATION_ACTION = 24;

const settings = phoenix.settings.define({
   pronounsFormat: {
      type: "select",
      title: "Format",
      description: "How pronouns appear in chat",
      options: [
         { value: "LOWERCASE", label: "Lowercase" },
         { value: "CAPITALIZED", label: "Capitalized" },
      ],
      default: "LOWERCASE",
      onChange: () => accessory.update?.(),
   },
   showSelf: {
      type: "switch",
      title: "Show my own",
      description: "Show pronouns on your own messages too",
      default: true,
      onChange: () => accessory.update?.(),
   },
});

interface ProfileResponse {
   user_profile?: { pronouns?: string };
   guild_member_profile?: { guild_id?: string; pronouns?: string };
}

// Equicord reads the profiles Discord's own store has loaded, which are the ones you opened. Fetching
// one per author would be a request for every name in a channel, so profiles are noted as the app
// loads them, and a name shows its pronouns once its profile has been opened.
const global = new Map<string, string>();
const inGuild = new Map<string, string>();
const guildOfChannel = new Map<string, string>();
let me: string | null = null;

phoenix.on(phoenix.gateway.ready, (ready) => {
   me = ready.user.id;
});
phoenix.discord
   .me()
   .then((user) => {
      me ??= user?.id ?? null;
   })
   .catch(() => {});

phoenix.on(phoenix.gateway.messageCreate, (message) => {
   if (message.guild_id) guildOfChannel.set(message.channel_id, message.guild_id);
});

phoenix.discord.onResponse({ method: "GET", pathSuffix: "/profile" }, (res) => {
   const userId = /^\/users\/(\d+)\/profile/.exec(res.path)?.[1];
   if (!userId || res.status !== 200) return;
   const profile = res.json<ProfileResponse>();
   if (!profile) return;
   const own = profile.user_profile?.pronouns?.trim();
   if (own) global.set(userId, own);
   else global.delete(userId);
   const guild = profile.guild_member_profile;
   if (guild?.guild_id) {
      const pronouns = guild.pronouns?.trim();
      if (pronouns) inGuild.set(`${userId}:${guild.guild_id}`, pronouns);
      else inGuild.delete(`${userId}:${guild.guild_id}`);
   }
   accessory.update?.();
});

const accessory = phoenix.ui.addMessageAccessory({
   placement: "header",
   render({ message }) {
      if (!message) return null;
      const author = message.author;
      if (author.bot || author.system || message.type === AUTO_MODERATION_ACTION) return null;
      if (!settings.showSelf && author.id === me) return null;
      const guildId = (message as { guild_id?: string }).guild_id ?? guildOfChannel.get(message.channel_id);
      const pronouns = (guildId ? inGuild.get(`${author.id}:${guildId}`) : undefined) ?? global.get(author.id);
      if (!pronouns) return null;
      const text = pronouns.replace(/\n+/g, "");
      return `• ${settings.pronounsFormat === "LOWERCASE" ? text.toLowerCase() : text}`;
   },
});
