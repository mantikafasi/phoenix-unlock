// @description Reacts with 🫃 to every message sent by @rgbcube
// @version 0.1.0
// @author peachey2k2

const { gateway } = phoenix;

const TARGET_ID = "512640455834337290"; // @rgbcube
const REACTION = encodeURIComponent("🫃");

phoenix.on(gateway.messageCreate, (message) => {
   if (message.author?.id !== TARGET_ID) return;
   if (!message.channel_id || !message.id) return;
   phoenix.discord
      .put(`/channels/${message.channel_id}/messages/${message.id}/reactions/${REACTION}/@me`)
      .then((res) => {
         if (!res.ok) phoenix.log.error(`failed to add reaction: HTTP ${res.status}`);
      })
      .catch((error) => phoenix.log.error("failed to add reaction:", error));
});
