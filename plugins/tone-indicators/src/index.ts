const settings = phoenix.settings.define({
   prefix: {
      type: "text",
      title: "Prefix",
      description: "Prefix character(s) for tone indicators",
      default: "/",
   },
   customIndicators: {
      type: "text",
      title: "Custom indicators",
      description: "Your own, as jk=Joking; srs=Serious",
      default: "",
   },
});

// A name starting with an underscore is an alias, and is matched without it.
const DEFAULTS: Record<string, string> = {
   j: "Joking",
   hj: "Half Joking",
   s: "Sarcastic",
   _sarc: "Sarcastic",
   gen: "Genuine",
   _g: "Genuine",
   genq: "Genuine Question",
   _gq: "Genuine Question",
   srs: "Serious",
   nsrs: "Non-Serious",
   pos: "Positive Connotation",
   _pc: "Positive Connotation",
   neu: "Neutral Connotation",
   neg: "Negative Connotation",
   _nc: "Negative Connotation",
   p: "Platonic",
   r: "Romantic",
   a: "Alterous",
   c: "Copypasta",
   l: "Lyrics",
   _ly: "Lyrics",
   lh: "Light-Hearted",
   nm: "Not Mad",
   lu: "A Little Upset",
   nbh: "Not Directed At Anybody Here",
   nay: "Not Directed At You",
   ay: "Directed At You",
   nsb: "Not Subtweeting",
   x: "Sexual Intent",
   _sx: "Sexual Intent",
   nx: "Non-Sexual Intent",
   _nsx: "Non-Sexual Intent",
   rh: "Rhetorical Question",
   _rt: "Rhetorical Question",
   t: "Teasing",
   ij: "Inside Joke",
   m: "Metaphorically",
   li: "Literally",
   hyp: "Hyperbole",
   ex: "Exaggeration",
   f: "Fake",
   q: "Quote",
   th: "Threat",
   cb: "Clickbait",
   ref: "Reference",
   nf: "Not Forced",
   nbr: "Not Being Rude",
   ot: "Off-Topic",
   nj: "Not Joking",
   naj: "Not A Joke",
};

const escape = (text: string) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

function indicators(): Map<string, string> {
   const all = new Map<string, string>();
   for (const [key, desc] of Object.entries(DEFAULTS)) all.set(key.replace(/^_/, ""), desc);
   for (const entry of settings.customIndicators.split("; ")) {
      const [key, ...rest] = entry.split("=");
      if (key.trim() && rest.length > 0) all.set(key.trim().toLowerCase().replace(/^_/, ""), rest.join("=").trim());
   }
   return all;
}

function describe(content: string): string[] {
   if (!content) return [];
   const all = indicators();
   const prefix = settings.prefix || "/";
   // Longest first, so "genq" is not read as "gen".
   const names = [...all.keys()].map(escape).sort((a, b) => b.length - a.length);
   // A slash is kept out of what may follow, so s/find/replace is not read as /find.
   const found = new RegExp(`(?:^|\\s)${escape(prefix)}(${names.join("|")})(?=\\s|$|[^\\s\\w/])`, "giu");
   const seen = new Set<string>();
   const out: string[] = [];
   for (const match of content.matchAll(found)) {
      const name = match[1].toLowerCase();
      const desc = all.get(name);
      if (desc && !seen.has(name)) {
         seen.add(name);
         out.push(`${prefix}${name}: ${desc}`);
      }
   }
   return out;
}

phoenix.ui.addMessageAccessory({
   render: ({ message }) => {
      const lines = describe(message?.content ?? "");
      return lines.length ? lines.join("  ·  ") : null;
   },
});
