// Equicord rewrites the host of the "Copy Message Link" menu row. Phoenix's copy-link row is
// native with no hook, so this normalizes the links in the messages you send instead, which
// also covers links pasted from a canary or ptb client.
const LINK = /https?:\/\/(?:canary\.|ptb\.)(discord(?:app)?\.com\/channels\/)/g;

phoenix.messages.onSend((text) => {
   const out = text.replace(LINK, "https://$1");
   return out === text ? undefined : out;
});
