export default {
   description: "Adds a Husk button to the message menu that reacts with it",
   version: "1.0.0",
   author: "nin0dev",
   permissions: {
      gateway: ["guildCreate", "guildEmojisUpdate", "messageCreate"],
      discord: ["PUT /channels/*/messages/*/reactions/*/@me"],
   },
} satisfies Phoenix.Manifest;
