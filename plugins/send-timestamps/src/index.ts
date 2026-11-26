const FORMATS = ["R", "t", "T", "d", "s", "S", "D", "f", "F"];

const settings = phoenix.settings.define({
   replaceMessageContents: {
      type: "switch",
      title: "Replace timestamps in messages",
      description: "Turns `HH:MM` in backticks, like `17:30` or `5:30 PM`, into a timestamp in your timezone",
      default: true,
   },
});

function parseTime(time: string): string {
   const clean = time.slice(1, -1).replace(/(\d)(AM|PM)$/i, "$1 $2");
   const date = new Date(`${new Date().toDateString()} ${clean}`);
   if (Number.isNaN(date.getTime())) return time;
   // Tomorrow can be 23 or 25 hours away across daylight saving changes.
   if (Date.now() > date.getTime()) date.setDate(date.getDate() + 1);
   return `<t:${Math.round(date.getTime() / 1000)}:t>`;
}

phoenix.messages.onSend((text) => {
   if (!settings.replaceMessageContents) return;
   const out = text.replace(/`\d{1,2}:\d{2} ?(?:AM|PM)?`/gi, parseTime);
   return out === text ? undefined : out;
});

// Equicord's date picker is a window with a calendar. Without one, the date is typed as text:
// `/timestamp 2026-12-25 18:00 F`, or `/timestamp 17:30`, and a format letter is optional.
phoenix.commands.register({
   name: "timestamp",
   description: `Sends a timestamp, like /timestamp 2026-12-25 18:00 F. Formats: ${FORMATS.join(" ")}`,
   execute(args, ctx) {
      const words = args.trim().split(/\s+/).filter(Boolean);
      const format = words.length > 1 && FORMATS.includes(words[words.length - 1]) ? words.pop() : "f";
      const input = words.join(" ");
      if (!input) {
         ctx.reply("Usage: /timestamp <date and time> [format]");
         return;
      }
      const time = /^\d{1,2}:\d{2}( ?(AM|PM))?$/i.test(input)
         ? new Date(`${new Date().toDateString()} ${input.replace(/(\d)(AM|PM)$/i, "$1 $2")}`)
         : new Date(input);
      if (Number.isNaN(time.getTime())) {
         ctx.reply(`Could not read "${input}" as a date`);
         return;
      }
      return `<t:${Math.round(time.getTime() / 1000)}:${format}>`;
   },
});
