const ICON = "res://ic_mana_puzzle_piece_plus";
const GREEN = 0xff23a55a;
const RED = 0xfff23f43;

interface WordMatch {
   word: string;
   startIndex: number;
}

const settings = phoenix.settings.define({
   showIcon: {
      type: "switch",
      title: "Show composer button",
      description: "A button to toggle the Ingtoninator",
      default: true,
      onChange: () => syncButton(),
   },
   isEnabled: {
      type: "switch",
      title: "Enabled",
      description: "Enable or disable the Ingtoninator",
      default: true,
      onChange(value) {
         setEnabled(value);
      },
   },
});

// The composer button flips this without being able to move the switch, the same split
// spoiler-mode has: the switch seeds the state, then the last one touched wins.
let enabled = phoenix.storage.get<boolean>("enabled") ?? settings.isEnabled;

const setEnabled = (value: boolean) => {
   enabled = value;
   phoenix.storage.set("enabled", value);
   syncButton();
};

const isLegal = (word: string): boolean => {
   if (word === "i" || word === "I") return false;
   return !["a", "e", "o", "u", "y"].includes(word.slice(-1).toLowerCase());
};

const getWords = (text: string): WordMatch[] => {
   const linked = new Set<number>();
   for (const match of text.matchAll(/https?:\/\/[^\s]+\.[^\s]+/g)) {
      const start = match.index ?? 0;
      for (let i = start; i < start + match[0].length; i++) linked.add(i);
   }
   return Array.from(text.matchAll(/\p{L}+/gu), (match) => ({
      word: match[0],
      startIndex: match.index ?? 0,
   })).filter((match) => !linked.has(match.startIndex));
};

const chooseRandomWord = (text: string): WordMatch | null => {
   const words = getWords(text);
   while (words.length > 0) {
      const index = Math.floor(Math.random() * words.length);
      if (isLegal(words[index].word)) return words[index];
      words.splice(index, 1);
   }
   return null;
};

const ington = (word: string): string => {
   if (word.endsWith("INGTON")) return "";
   if (word.endsWith("INGTO")) return "N";
   if (word.endsWith("INGT")) return "ON";
   if (word.endsWith("ING")) return "TON";
   if (word.endsWith("IN")) return "GTON";
   if (word.endsWith("I")) return "NGTON";
   return "INGTON";
};

const label = () => (enabled ? "Ingtoninator Enabled" : "Ingtoninator Disabled");

// Equicord hides the button with showIcon and restarts. Here it is added and removed live.
let button: Phoenix.ButtonRemover | null = null;

const syncButton = () => {
   if (!settings.showIcon) {
      button?.();
      button = null;
      return;
   }
   const patch = { icon: ICON, label: label(), tint: enabled ? GREEN : RED };
   if (button) {
      button.update(patch);
      return;
   }
   button = phoenix.ui.addComposerButton({
      ...patch,
      onPress() {
         setEnabled(!enabled);
         phoenix.ui.toast(label());
      },
   });
};
syncButton();

phoenix.messages.onSend((text) => {
   if (!enabled || !text.trim()) return;
   const match = chooseRandomWord(text);
   if (!match) return;
   const upper = match.word.toUpperCase();
   let insertion = ington(upper);
   if (match.word !== upper) insertion = insertion.toLowerCase();
   const at = match.startIndex + match.word.length;
   return text.slice(0, at) + insertion + text.slice(at);
});
