export default {
   description: "Shows pronouns next to the name on messages, for profiles you have opened",
   version: "1.0.0",
   author: "Tyman, TheKodeToad, Ven, Elvyra",
   permissions: {
      gateway: ["messageCreate", "ready"],
      discord: ["GET /users/@me", "GET /users/*/profile"],
   },
} satisfies Phoenix.Manifest;
