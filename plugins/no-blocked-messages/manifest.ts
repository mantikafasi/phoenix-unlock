export default {
   description: "Hides messages from blocked (and optionally ignored) users completely",
   version: "1.1.0",
   author: "Nachtasyl, rushii, Samu, jamesbt365, Elvyra, Etorix",
   permissions: {
      gateway: ["messageCreate", "messageUpdate", "relationshipAdd", "relationshipUpdate", "relationshipRemove"],
      discord: ["GET /users/@me/relationships", "GET /channels/*/messages"],
   },
} satisfies Phoenix.Manifest;
