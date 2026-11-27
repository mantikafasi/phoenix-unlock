export default {
   description: "Notifies you when you lose a friend, a friend request, a server or a group",
   version: "1.0.0",
   author: "acasamit",
   permissions: {
      gateway: [
         "relationshipAdd",
         "relationshipUpdate",
         "relationshipRemove",
         "guildCreate",
         "guildDelete",
         "channelDelete",
         "channelRecipientRemove",
         "readySupplemental",
      ],
      discord: [
         // Who is signed in, and what Discord lists now before reporting a removal.
         "GET /users/@me",
         "GET /users/@me/relationships",
         "GET /users/@me/guilds",
         "GET /users/@me/channels",
         // Only watched with onRequest, so your own removals are not reported. Never sent.
         "DELETE /users/@me/relationships/*",
         "PUT /users/@me/relationships/*",
         "DELETE /users/@me/guilds/*",
         "DELETE /guilds/*",
         "POST /guilds/*/delete",
         "DELETE /channels/*",
      ],
      stores: true,
   },
} satisfies Phoenix.Manifest;
