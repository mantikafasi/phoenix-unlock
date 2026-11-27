const settings = phoenix.settings.define({
   alwaysPingOnReply: {
      type: "switch",
      title: "Always ping on reply",
      description: "Always get pinged when someone replies to your messages",
      default: false,
   },
   replyPingWhitelist: {
      type: "text",
      title: "Always ping from",
      description: "Comma-separated user IDs to always receive reply pings from",
      default: "",
   },
   replyPingBlacklist: {
      type: "text",
      title: "Never ping from",
      description: "Comma-separated user IDs to never receive reply pings from",
      default: "",
   },
});

let me: Phoenix.User | null = null;

phoenix.on(phoenix.gateway.ready, (ready) => {
   me = ready.user;
});
phoenix.discord
   .me()
   .then((user) => {
      me ??= user;
   })
   .catch(() => {});

const listed = (list: string, id: string) => list.split(",").some((entry) => entry.trim() === id);

phoenix.hook(phoenix.gateway.messageCreate, {
   before(message) {
      if (!me || message.author.id === me.id) return;
      // Equicord looks the replied message up in its store. Phoenix sends it with the reply.
      const replied = message.referenced_message;
      if (replied?.author.id !== me.id) return;

      const authorId = message.author.id;
      const mentioned = message.mentions.some((user) => user.id === me?.id);
      if (listed(settings.replyPingBlacklist, authorId)) {
         message.mentions = message.mentions.filter((user) => user.id !== me?.id);
      } else if (settings.alwaysPingOnReply || listed(settings.replyPingWhitelist, authorId)) {
         if (!mentioned) message.mentions.push(me);
      } else {
         message.mentions = message.mentions.filter((user) => user.id !== me?.id);
      }
   },
});
