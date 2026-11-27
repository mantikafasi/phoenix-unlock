export default {
   description: "Splits oversized messages into Discord-sized chunks before sending",
   version: "1.0.0",
   author: "Reycko, justjxke",
   permissions: {
      gateway: ["ready"],
      discord: ["GET /users/@me", "GET /channels/*", "POST /channels/*/messages"],
      messages: true,
   },
} satisfies Phoenix.Manifest;
