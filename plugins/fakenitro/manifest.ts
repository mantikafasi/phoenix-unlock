export default {
   description:
      "Unlocks Nitro stream quality, themes and app icons, sends custom emojis and stickers as links, and renders fake emojis, stickers, and GIFs as real ones",
   version: "0.7.0",
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
         "GET /users/@me/settings-proto/1",
         "PATCH /users/@me/settings-proto/1",
      ],
      stores: true,
      messages: true,
   },
} satisfies Phoenix.Manifest;
