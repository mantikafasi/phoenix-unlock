export default {
   description: "Repeats a message, with or without replying to it, from the message menu",
   version: "1.0.0",
   author: "Tolgchu, thororen",
   permissions: {
      discord: ["POST /channels/*/messages"],
   },
} satisfies Phoenix.Manifest;
