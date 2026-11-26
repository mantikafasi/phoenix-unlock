export default {
   description: "Control whether you always or never get pinged on replies, with an allow and block list",
   version: "1.0.0",
   author: "ant0n, MrDiamond, keircn",
   permissions: {
      gateway: ["messageCreate", "ready"],
      discord: ["GET /users/@me"],
   },
} satisfies Phoenix.Manifest;
