const settings = phoenix.settings.define({
   quickDisable: {
      type: "switch",
      title: "Quick disable",
      description: "Turns off message modifying without reloading the plugin",
      default: false,
   },
   blockedWords: {
      type: "text",
      title: "Words not to capitalize",
      description: "Comma separated",
      default: "",
   },
   fixApostrophes: {
      type: "switch",
      title: "Fix apostrophes",
      description: "Ensure contractions contain apostrophes",
      default: true,
   },
   expandContractions: {
      type: "switch",
      title: "Expand contractions",
      description: "Write out contractions in full",
      default: false,
   },
   fixCapitalization: {
      type: "switch",
      title: "Capitalize sentences",
      description: "Capitalize the first letter of each sentence",
      default: false,
   },
   fixPunctuation: {
      type: "switch",
      title: "Punctuate sentences",
      description: "End each line with a period",
      default: false,
   },
   fixPunctuationFrequency: {
      type: "number",
      title: "Period frequency (0 to 100)",
      description: "Percent of messages that get periods, this majorly annoys some people",
      default: 100,
   },
});

// Both adding apostrophes and expanding contractions work from this table.
const contractionsMap: Record<string, string> = {
   "wasn't": "was not",
   "can't": "cannot",
   "don't": "do not",
   "won't": "will not",
   "isn't": "is not",
   "aren't": "are not",
   "haven't": "have not",
   "hasn't": "has not",
   "hadn't": "had not",
   "doesn't": "does not",
   "didn't": "did not",
   "shouldn't": "should not",
   "wouldn't": "would not",
   "couldn't": "could not",
   "that's": "that is",
   "what's": "what is",
   "there's": "there is",
   "how's": "how is",
   "where's": "where is",
   "when's": "when is",
   "who's": "who is",
   "why's": "why is",
   "you'll": "you will",
   "i'll": "I will",
   "they'll": "they will",
   "it'll": "it will",
   "i'm": "I am",
   "you're": "you are",
   "they're": "they are",
   "he's": "he is",
   "she's": "she is",
   "i've": "I have",
   "you've": "you have",
   "we've": "we have",
   "they've": "they have",
   "you'd": "you would",
   "he'd": "he would",
   "she'd": "she would",
   "it'd": "it would",
   "we'd": "we would",
   "they'd": "they would",
   "y'all": "you all",
   "here's": "here is",
};

const missingApostropheMap: Record<string, string> = {};
for (const contraction of Object.keys(contractionsMap)) {
   missingApostropheMap[contraction.replace(/'/g, "")] = contraction;
}

const own = (map: Record<string, string>, key: string) => Object.prototype.hasOwnProperty.call(map, key);

function getCapData(text: string): boolean[] {
   const data: boolean[] = [];
   for (const char of text) {
      if (/[a-zA-Z]/.test(char)) data.push(char === char.toUpperCase());
   }
   return data;
}

function restoreCap(text: string, data: boolean[]): string {
   let out = "";
   let at = 0;
   for (const char of text) {
      if (!/[a-zA-Z]/.test(char)) {
         out += char;
         continue;
      }
      out += data[at] ? char.toUpperCase() : char.toLowerCase();
      // A shorter source keeps its last case for the rest.
      if (at < data.length - 1) at++;
   }
   return out;
}

function ensureApostrophe(text: string): string {
   const find = new RegExp(`\\b(${Object.keys(missingApostropheMap).join("|")})\\b`, "gi");
   return text.replace(find, (match) => {
      const lower = match.toLowerCase();
      return own(missingApostropheMap, lower) ? restoreCap(missingApostropheMap[lower], getCapData(match)) : match;
   });
}

function expandContractions(text: string): string {
   const find = new RegExp(`\\b(${Object.keys(contractionsMap).join("|")})\\b`, "gi");
   return text.replace(find, (match) => {
      const lower = match.toLowerCase();
      return own(contractionsMap, lower) ? restoreCap(contractionsMap[lower], getCapData(match)) : match;
   });
}

// Safe against catastrophic backtracking, which matters since it runs on every send.
const SENTENCE_SPLIT = /((?<!\w\.\w.)(?<!\b[A-Z][a-z]\.)(?<![A-Z]\.)(?<!\.)(?<=[.?!])\s+|\n+)/;

function capitalize(text: string): string {
   const blocked = settings.blockedWords
      .split(/,\s?/)
      .filter(Boolean)
      .map((word) => word.toLowerCase());

   let result = "";
   // The split keeps its delimiters, so sentences and separators alternate.
   for (const part of text.split(SENTENCE_SPLIT)) {
      if (!part) continue;
      if (SENTENCE_SPLIT.test(part) || part.trim() === "") {
         result += part;
         continue;
      }
      const firstWord = /^\s*([\w'-]+)/.exec(part)?.[1].toLowerCase() ?? "";
      if (blocked.includes(firstWord) || part.startsWith("http")) {
         result += part;
      } else {
         result += part.replace(/^(\s*)(\S)/, (_, space: string, first: string) => space + first.toUpperCase());
      }
   }
   return result.replace(/\bi\b(?!\s+is\b)(?=['\s]|$)/g, "I");
}

function addPeriods(text: string): string {
   if (!text) return "";
   const lines = text.split("\n");
   const out: string[] = [];
   for (const [i, line] of lines.entries()) {
      const stripped = line.trimEnd();
      if (!stripped) {
         if (i < lines.length - 1) out.push("");
         continue;
      }
      const endsInLink = /https?:\/\/\S+$|www\.\S+$/.test(stripped);
      out.push(/[A-Za-z0-9]/.test(stripped.slice(-1)) && !endsInLink ? `${stripped}.` : stripped);
   }
   return out.join("\n");
}

function polish(input: string): string {
   if (settings.quickDisable) return input;
   // Code stays as typed.
   const code: string[] = [];
   let text = input.replace(/```[\s\S]*?```|`[\s\S]*?`/g, (match) => {
      code.push(match);
      return `__CODE_BLOCK_${code.length - 1}__`;
   });
   // Contractions are fixed before they are expanded.
   if (settings.fixApostrophes || settings.expandContractions) text = ensureApostrophe(text);
   if (settings.fixCapitalization) text = capitalize(text);
   if (settings.fixPunctuation && Math.random() * 100 < settings.fixPunctuationFrequency) text = addPeriods(text);
   if (settings.expandContractions) text = expandContractions(text);
   return text.replace(/__CODE_BLOCK_(\d+)__/g, (_, index: string) => code[Number(index)]);
}

phoenix.messages.onSend((text) => {
   const out = polish(text);
   return out === text ? undefined : out;
});
