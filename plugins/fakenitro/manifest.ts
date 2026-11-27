export default {
   description: "Sends custom and animated emojis as links, and renders fake emojis, stickers, and GIFs as real ones",
   version: "0.4.4",
   author: "mantikafasi",
   permissions: {
      gateway: [
         "ready",
         "readySupplemental",
         "userUpdate",
         "guildCreate",
         "channelCreate",
         "guildEmojisUpdate",
         "guildStickersUpdate",
         "messageCreate",
         "messageUpdate",
      ],
      discord: [
         "GET /users/@me",
         "POST /channels/*/messages",
         "GET /channels/*/messages",
         "PATCH /channels/*/messages/*",
         "GET /channels/*/pins",
         "GET /channels/*/messages/search",
         "GET /guilds/*/messages/search",
      ],
      stores: true,
      messages: true,
   },
} satisfies Phoenix.Manifest;
