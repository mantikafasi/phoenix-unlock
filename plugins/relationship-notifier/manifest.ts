export default {
   description: "Notifies you when you lose a friend, a friend request, a server or a group",
   version: "1.0.0",
   author: "acasamit",
   // Hooks and the cache are `gateway`; confirming a removal and watching your own
   // removals go through `phoenix.discord`.
   permissions: ["gateway", "discord"],
} satisfies Phoenix.Manifest;
