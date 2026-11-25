// @description Removes tracking parameters from links you send and receive
// @version 0.1.2
// @author amaanq

const { gateway } = phoenix;

const TRACKERS = [
  /^utm_/,
  /^(fbclid|gclid|dclid|gbraid|wbraid|msclkid|yclid|twclid|ttclid|igshid|igsh|mc_eid|mc_cid)$/,
  /^(_hsenc|_hsmi|mkt_tok|oly_anon_id|oly_enc_id|vero_id|wickedid|__s)$/,
];

// Short parameter names can carry functional values on unrelated sites.
const SHORT_PARAM_HOSTS = /(^|\.)(twitter\.com|x\.com|tiktok\.com)$/;
const TWITTER_HOSTS = /(^|\.)(twitter\.com|x\.com)$/;
const YOUTUBE_HOSTS = /(^|\.)(youtube\.com|youtu\.be)$/;
const SPOTIFY_HOSTS = /(^|\.)spotify\.com$/;

const isTracker = (key: string, host: string) => {
  if (TRACKERS.some((tracker) => tracker.test(key))) return true;
  if (key === "s" || key === "t") return SHORT_PARAM_HOSTS.test(host);
  if (key === "ref_src" || key === "ref_url") return TWITTER_HOSTS.test(host);
  if (key === "si") return YOUTUBE_HOSTS.test(host) || SPOTIFY_HOSTS.test(host);
  return (key === "feature" || key === "pp") && YOUTUBE_HOSTS.test(host);
};

const clean = (url: string) => {
  const query = url.indexOf("?");
  const hash = url.indexOf("#");
  if (query === -1 || (hash !== -1 && hash < query)) return url;
  const end = hash === -1 ? url.length : hash;
  const authority = url.slice(url.indexOf("//") + 2).split(/[/?#]/)[0];
  const host = authority.slice(authority.lastIndexOf("@") + 1).split(":")[0].toLowerCase().replace(/\.$/, "");
  const pairs = url.slice(query + 1, end).split("&");
  const kept = pairs.filter((pair) => {
    let key: string;
    try {
      key = decodeURIComponent(pair.split("=")[0]);
    } catch {
      return true;
    }
    return !isTracker(key, host);
  });
  if (kept.length === pairs.length) return url;
  return url.slice(0, query) + (kept.some(Boolean) ? `?${kept.join("&")}` : "") + url.slice(end);
};

const cleanLinks = (text: string) => text.replace(/https?:\/\/(?:[^\s<>()`|]|\([^\s<>()`|]*\))+/gi, (url) => {
  const suffix = url.match(/(?:[.,!?;:\]]|\*\*|~~)+$/)?.[0] ?? "";
  const link = suffix ? url.slice(0, -suffix.length) : url;
  return clean(link) + suffix;
});

const rewrite = (message: { content?: string }) => {
  if (typeof message.content !== "string" || !message.content.includes("?")) return;
  message.content = cleanLinks(message.content);
};

phoenix.hook(gateway.messageCreate, { before: rewrite });
phoenix.hook(gateway.messageUpdate, { before: rewrite });
phoenix.messages.onSend((text) => (text.includes("?") ? cleanLinks(text) : undefined));
