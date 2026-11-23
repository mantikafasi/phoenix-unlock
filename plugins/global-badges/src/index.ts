// @description Badges from other client mods, shown in the profile and badge directory
// @version 0.3.0
// @author iFlex0x

const API = "https://api.domi-btnr.dev/clientmodbadges";
const DONORS = "https://badges.vencord.dev/badges.json";
const TTL = 15 * 60 * 1000;
const DONOR_TTL = 30 * 60 * 1000;
const CACHE_CAP = 64;

type RawBadge = string | { name: string; badge: string };
type Badges = Record<string, RawBadge[]>;
type Donor = { tooltip?: string; badge: string };
type ShownBadge = { uri: string; label: string; description: string; source: string };

const settings = phoenix.settings.define({
  showPrefix: {
    type: "switch",
    title: "Client mod as prefix",
    description: "Show the mod name before each badge",
    default: true,
  },
  showCustom: {
    type: "switch",
    title: "Custom badges",
    description: "Show badges a mod defines itself",
    default: true,
  },
});

const FULL_NAMES: Record<string, string> = { hunter: "Bug Hunter", early: "Early User" };

// vencord redesigned this in oct 2026; the clientmod api still serves the old art
const VENCORD_CONTRIBUTOR = "https://cdn.discordapp.com/emojis/1562267648857997356.webp?size=128";

const cache = new Map<string, { at: number; badges: ShownBadge[] }>();
let donors: { at: number; byUser: Record<string, Donor[]> } | null = null;

function badgeOf(mod: string, badge: RawBadge): ShownBadge | null {
  if (typeof badge !== "string") {
    if (!settings.showCustom) return null;
    const name = badge?.name?.trim();
    const uri = badge?.badge?.trim();
    if (!name || !uri) return null;
    return { uri, label: name, description: `Custom badge from ${mod}.`, source: mod };
  }

  const slug = badge.replace(mod, "").trim().split(" ")[0];
  if (!slug) return null;

  const clean = (FULL_NAMES[badge] ?? badge).replace(mod, "").trim();
  if (!clean) return null;

  const name = clean.charAt(0).toUpperCase() + clean.slice(1);
  return {
    uri:
      mod === "Vencord" && slug.toLowerCase() === "contributor"
        ? VENCORD_CONTRIBUTOR
        : `${API}/badges/${mod}/${slug}`,
    label: settings.showPrefix ? `${mod} ${name}` : name,
    description:
      slug.toLowerCase() === "contributor"
        ? `For contributing to ${mod}.`
        : `${mod} badge.`,
    source: mod,
  };
}

async function donorBadgesFor(userId: string): Promise<ShownBadge[]> {
  if (!donors || Date.now() - donors.at >= DONOR_TTL) {
    try {
      const res = await phoenix.fetch(DONORS);
      donors = { at: Date.now(), byUser: res.ok ? res.json<Record<string, Donor[]>>() : {} };
    } catch {
      donors = { at: Date.now(), byUser: {} };
    }
  }

  const list = donors.byUser?.[userId];
  if (!Array.isArray(list)) return [];
  return list
    .map((one): ShownBadge | null => {
      const uri = one?.badge?.trim();
      if (!uri) return null;
      return {
        uri,
        label: one.tooltip?.trim() || "Vencord Donor",
        description: "A badge for supporting Vencord.",
        source: "Vencord Donor",
      };
    })
    .filter((one): one is ShownBadge => one !== null);
}

async function badgesFor(userId: string): Promise<ShownBadge[]> {
  const hit = cache.get(userId);
  if (hit && Date.now() - hit.at < TTL) return hit.badges;

  let body: Badges = {};
  try {
    const res = await phoenix.fetch(`${API}/users/${userId}`);
    if (res.ok) body = res.json<Badges>() ?? {};
  } catch {
    body = {};
  }

  const badges = await donorBadgesFor(userId);
  for (const [mod, list] of Object.entries(body)) {
    if (!Array.isArray(list)) continue;
    for (const badge of list) {
      const one = badgeOf(mod, badge);
      if (one) badges.push(one);
    }
  }

  // the clientmod API also lists donors under Vencord; those are already here
  const seen = new Set<string>();
  const unique = badges.filter(one => {
    if (seen.has(one.uri)) return false;
    seen.add(one.uri);
    return true;
  });

  if (cache.size > CACHE_CAP) cache.clear();
  cache.set(userId, { at: Date.now(), badges: unique });
  return unique;
}

phoenix.ui.addProfileBadges({
  render: ({ userId }) => (userId ? badgesFor(userId) : []),
});
