const settings = phoenix.settings.define({
   myUnits: {
      type: "select",
      title: "Units I use",
      description: "The units you want things converted to",
      options: [
         { value: "imperial", label: "Imperial" },
         { value: "metric", label: "Metric" },
      ],
      default: "imperial",
   },
});

interface Conversion {
   regex: RegExp;
   convert(...groups: string[]): string;
}

// Matches imperial units and gives metric.
const IMPERIAL: Record<string, Conversion> = {
   fahrenheit: {
      regex: /(-?\d+(?:\.\d+)?)°?(f)(?!\w)/gi,
      convert: (...g) => `${((parseFloat(g[1]) - 32) * (5 / 9)).toFixed(2)}°C`,
   },
   feetInchesMark: {
      regex: /(\d+)(') ?(\d+(?:\.\d+)?)("|'')?/g,
      convert: (...g) => `${(parseFloat(g[1]) / 3.281 + parseFloat(g[3]) / 39.37).toFixed(2)}m`,
   },
   feetWord: {
      regex: /(\d+(?:\.\d+)?) *(f(ee)?t)(?! *\d)/gi,
      convert: (...g) => `${(parseFloat(g[1]) / 3.281).toFixed(2)}m`,
   },
   inchesWord: {
      regex: /(?<!\d+ *(?:f(?:ee|oo)?t) *)(\d+(?:\.\d+)?) *(in(?:ches?)?)/gi,
      // Equicord divides by 2.54 here, which turns inches into less than a centimeter.
      convert: (...g) => `${(parseFloat(g[1]) * 2.54).toFixed(2)}cm`,
   },
   feetInchesWord: {
      regex: /(\d+) *(f(?:ee|oo)?t) *(\d+(?:\.\d+)?) *(in(?:ches?)?)/gi,
      convert: (...g) => `${(parseFloat(g[1]) / 3.281 + parseFloat(g[3]) / 39.37).toFixed(2)}m`,
   },
   poundWord: {
      regex: /(\d+(?:\.\d+)?) *(lbs?|pounds?)\b(?! ?\d)/gi,
      convert: (...g) => `${(parseFloat(g[1]) / 2.205).toFixed(2)}kg`,
   },
   poundOunceWord: {
      regex: /(\d+(?:\.\d+)?) *(lbs?|pounds?) *(\d+(?:\.\d+)?) *(ozs?|ounces?)/gi,
      // The ounces are group 3, where Equicord read group 2, the unit text.
      convert: (...g) => `${(parseFloat(g[1]) / 2.205 + parseFloat(g[3]) / 35.274).toFixed(2)}kg`,
   },
   ounceWord: {
      regex: /(\d+(?:\.\d+)?) ?(ounces?|oz)/gi,
      convert: (...g) => `${(parseFloat(g[1]) * 28.35).toFixed(2)}g`,
   },
   milesPerHour: {
      regex: /(\d+(?:\.\d+)?) ?(m(?:p|\/)h)/gi,
      convert: (...g) => `${(parseFloat(g[1]) * 1.609).toFixed(2)}km/h`,
   },
};

// Matches metric units and gives imperial.
const METRIC: Record<string, Conversion> = {
   celsius: {
      regex: /(-?\d+(?:\.\d+)?)\s?°?c(?!\w)/gi,
      convert: (...g) => `${(parseFloat(g[1]) * (9 / 5) + 32).toFixed(2)}°F`,
   },
   centimeters: {
      regex: /(\d+(?:\.\d+)?) ?(cm|centimeters?)(?!\w)/gi,
      convert: (...g) => `${(parseFloat(g[1]) / 2.54).toFixed(2)}in`,
   },
   meters: {
      regex: /(\d+(?:\.\d+)?) ?(m|meters?)(?!\w)/gi,
      convert(...g) {
         const feet = parseFloat(g[1]) * 3.281;
         const whole = Math.floor(feet);
         const inches = (feet - whole) * 12;
         return inches < 0.005 ? `${whole}ft` : `${whole}ft ${inches.toFixed(2)}in`;
      },
   },
   kilometers: {
      regex: /(\d+(?:\.\d+)?) ?(km|kilometers?|kms?)(?!\w)/gi,
      convert: (...g) => `${(parseFloat(g[1]) / 1.609).toFixed(2)}mi`,
   },
   grams: {
      regex: /(\d+(?:\.\d+)?) ?(grams?|g)/gi,
      convert: (...g) => `${(parseFloat(g[1]) / 28.35).toFixed(2)}oz`,
   },
   kilograms: {
      regex: /(\d+(?:\.\d+)?) ?(kg|kilo(?:gram)?s?)/gi,
      convert: (...g) => `${(parseFloat(g[1]) * 2.205).toFixed(2)}lb`,
   },
   kilometersPerHour: {
      regex: /(\d+(?:\.\d+)?) ?(km\/h|kmph|kph|kilometers?\/?h)/gi,
      convert: (...g) => `${(parseFloat(g[1]) / 1.609).toFixed(2)}mph`,
   },
};

function convert(message: string): string {
   const table = settings.myUnits === "imperial" ? METRIC : IMPERIAL;
   let out = message;
   for (const unit of Object.values(table)) {
      // A number is read whole, so "10 in" is not taken for "0 in" after a 1.
      const regex = new RegExp(`(?<![\\d.])${unit.regex.source}`, unit.regex.flags);
      out = out.replaceAll(regex, (...args) => unit.convert(...(args.slice(0, -2) as string[])));
   }
   return out;
}

// Converted text by message, until the plugin is reloaded.
const converted = new Map<string, string>();

const accessory = phoenix.ui.addMessageAccessory({
   render: ({ messageId }) => converted.get(messageId) ?? null,
});

phoenix.ui.addMessageAction({
   label: "Convert Units",
   icon: "res://ic_mana_angle_brackets",
   visible: (ctx) => Boolean(ctx.message?.content),
   onPress(ctx) {
      const text = ctx.message?.content;
      if (!text) return;
      const out = convert(text);
      // A message with no units has nothing to show.
      if (out === text) {
         phoenix.ui.toast("No units to convert");
         return;
      }
      converted.set(ctx.messageId, out);
      accessory.update?.(ctx.messageId);
   },
});
