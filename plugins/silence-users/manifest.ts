export default {
   description: "Silences @mention pings from chosen users, while their messages stay visible",
   version: "1.0.0",
   author: "dka",
   permissions: {
      gateway: ["messageCreate", "messageUpdate"],
   },
} satisfies Phoenix.Manifest;
