export default {
   description: "Allows the user to choose preferred video and audio codecs for streaming",
   version: "1.0.0",
   author: "xx8008",
   enabled: false,
   permissions: {
      gatewaySend: [4],
      discord: ["* /streams/**", "* /voice/**", "* /channels/*/call/**"],
   },
} satisfies Phoenix.Manifest;
