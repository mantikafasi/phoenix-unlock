const settings = phoenix.settings.define({
   regexByDefault: {
      type: "switch",
      title: "Regex by default",
      description: "Inverts the `r` flag: `r` then means plain text, and leaving it out means regex",
      default: false,
   },
});

const SED =
   /^s(?<sep>[/|$#@!])(?<match>(?!\1)(?:(?![^\\]\1).)*.|)\1(?<replace>(?!\1)(?:(?![^\\]\1).)*.|)\1?(?<modes>[rgmisudyv]*)$/;
const REGEX_FLAGS = "gmisudyv";
// Message types that can be edited: default and reply.
const EDITABLE = new Set([0, 19]);

// Equicord edits the pending reply target when one is set, since Phoenix's reply draft is not
// readable here, the last editable message of yours in the open channel is always the target.
async function lastEditable(channelId: string, userId: string): Promise<Phoenix.Message | null> {
   const res = await phoenix.discord.get(`/channels/${channelId}/messages?limit=50`);
   if (!res.ok) return null;
   const messages = res.json<Phoenix.Message[]>();
   return messages.find((m) => m.author.id === userId && EDITABLE.has(m.type)) ?? null;
}

async function run(text: string, channelId: string, userId: string | null, ctx: Phoenix.UiContext) {
   const groups = SED.exec(text)?.groups;
   if (!groups) return;
   let { match: pattern, replace } = groups;
   const flags = groups.modes.split("");
   const id = userId ?? (await phoenix.discord.me())?.id;
   if (!id) return;
   const target = await lastEditable(channelId, id);
   if (!target) return;

   const isRegex = flags.includes("r") !== settings.regexByDefault;
   let find: string | RegExp = pattern;
   if (isRegex) {
      try {
         find = new RegExp(pattern, flags.filter((f) => REGEX_FLAGS.includes(f)).join(""));
      } catch {
         ctx.reply("sed: invalid regular expression");
         return;
      }
   } else {
      const unescape = /\\([*?+/])/g;
      pattern = pattern.replace(unescape, (_, x) => x);
      replace = replace.replace(unescape, (_, x) => x);
      find = pattern;
   }
   // A global replace needs a global regex, and a string pattern only replaces its first hit.
   const replaced = flags.includes("g")
      ? find instanceof RegExp && !find.global
         ? target.content.replace(new RegExp(find.source, `${find.flags}g`), replace)
         : target.content.replaceAll(find, replace)
      : target.content.replace(find, replace);

   const path = `/channels/${channelId}/messages/${target.id}`;
   if (!replaced.trim() && target.attachments.length === 0) {
      await phoenix.discord.delete(path);
   } else if (replaced !== target.content) {
      await phoenix.discord.patch(path, { content: replaced });
   }
}

phoenix.messages.onSend((text, ctx) => {
   const channelId = ctx.channelId;
   if (!channelId || !SED.test(text)) return;
   run(text, channelId, ctx.currentUserId, ctx).catch((error) => phoenix.log.error("sed failed", error));
   // Nothing is posted, the edit replaces the command.
   return false;
});
