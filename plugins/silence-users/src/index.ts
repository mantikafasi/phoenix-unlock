const settings = phoenix.settings.define({
   mutedUserIds: {
      type: "text",
      title: "Silenced user IDs",
      description: "Comma-separated user IDs whose pings are silenced",
      default: "",
   },
});

const mutedIds = (): Set<string> =>
   new Set(
      settings.mutedUserIds
         .split(",")
         .map((id) => id.trim())
         .filter(Boolean)
   );

// Clearing the mention fields keeps the message but takes away what makes it ping. Equicord also
// drops the notification event, which Phoenix has no hook for, so a notification is the one
// thing this cannot stop.
function silence(message: Phoenix.GatewayMessage) {
   if (!mutedIds().has(message.author?.id)) return;
   message.mention_everyone = false;
   message.mention_roles = [];
   message.mentions = [];
}

phoenix.hook(phoenix.gateway.messageCreate, { before: silence });
phoenix.hook(phoenix.gateway.messageUpdate, { before: silence });
