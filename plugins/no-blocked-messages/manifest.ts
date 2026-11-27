export default {
   description: "Hides messages from blocked (and optionally ignored) users completely",
   version: "1.0.2",
   author: "Nachtasyl",
   permissions: {
      gateway: [
         "messageCreate",
         "messageUpdate",
         "relationshipAdd",
         "relationshipUpdate",
         "relationshipRemove",
      ],
      discord: ["GET /**"],
   },
} satisfies Phoenix.Manifest;
