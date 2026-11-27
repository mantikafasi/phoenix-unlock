export default {
   description: "Expands on Discord's rudimentary `s/find/replace/` message editing",
   version: "1.0.0",
   author: "dawn, Willow, kat",
   permissions: {
      discord: ["GET /channels/*/messages", "PATCH /channels/*/messages/*", "DELETE /channels/*/messages/*"],
      messages: true,
   },
} satisfies Phoenix.Manifest;
