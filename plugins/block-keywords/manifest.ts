export default {
   description: "Hides messages containing keywords you choose, as if their sender were blocked",
   version: "1.0.0",
   author: "catcraft, secp192k1",
   permissions: {
      gateway: ["messageCreate", "messageUpdate"],
      discord: ["GET /channels/*/messages"],
   },
} satisfies Phoenix.Manifest;
