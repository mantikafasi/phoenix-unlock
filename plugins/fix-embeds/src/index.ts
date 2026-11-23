// @description Rewrites the X/Twitter links you send so they get a rich embed
// @version 1.0.0
// @author cmdr-chara

const settings = phoenix.settings.define({
  twitter: {
    type: "switch",
    title: "X / Twitter",
    description: "Rewrite to fixupx.com",
    default: true,
  },
});

const MIRRORS: { host: RegExp; to: string; on: keyof typeof settings }[] = [
  { host: /^(www\.|mobile\.)?(twitter\.com|x\.com)$/i, to: "fixupx.com", on: "twitter" },
];

const fix = (text: string) =>
  text.replace(/https?:\/\/[^\s<>()]+/g, (url) => {
    const match = /^https?:\/\/([^/?#:]+)/i.exec(url);
    if (!match) return url;
    for (const { host, to, on } of MIRRORS) {
      if (settings[on] && host.test(match[1])) return url.replace(match[1], to);
    }
    return url;
  });

phoenix.messages.onSend((text) => (text.includes("http") ? fix(text) : undefined));
