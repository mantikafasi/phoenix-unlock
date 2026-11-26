const MESSAGE_LIMIT = 2000;
const NITRO_MESSAGE_LIMIT = 4000;
const SLOWMODE_BUFFER_MS = 250;

type SplitMode = "characters" | "spaces" | "newlines";

const settings = phoenix.settings.define({
   sendDelay: {
      type: "number",
      title: "Delay between parts (seconds)",
      description: "The least time between each part, from 0.5 to 10",
      default: 1,
   },
   splitMode: {
      type: "select",
      title: "Split at",
      description: "The boundary to prefer when splitting a message",
      options: [
         { value: "newlines", label: "Newlines" },
         { value: "spaces", label: "Spaces" },
         { value: "characters", label: "Strict character limit" },
      ],
      default: "newlines",
   },
   splitInSlowmode: {
      type: "switch",
      title: "Split in slowmode",
      description: "Allow splitting where slowmode is within the maximum below",
      default: false,
   },
   slowmodeMax: {
      type: "number",
      title: "Slowmode maximum (seconds)",
      description: "The longest slowmode to split in, from 1 to 30",
      default: 5,
   },
});

// On Desktop a paste over 4000 characters becomes a file before the box sees it. Holding this
// keeps it text, so it can be split here. It does nothing on a phone.
phoenix.messages.allowLongMessages?.();

let premium = false;
phoenix.on(phoenix.gateway.ready, (ready) => {
   premium = ready.user.premium_type === 2;
});
phoenix.discord
   .me()
   .then((user) => {
      premium ||= user?.premium_type === 2;
   })
   .catch(() => {});

const clamp = (value: number, min: number, max: number) => Math.min(max, Math.max(min, value));

function findSplitIndex(content: string, limit: number, mode: SplitMode): number {
   if (mode === "characters") return limit;
   const boundary = mode === "newlines" ? "\n" : " ";
   const preferred = content.lastIndexOf(boundary, limit - 1);
   if (preferred > 0) return preferred + boundary.length;
   if (mode === "newlines") {
      const space = content.lastIndexOf(" ", limit - 1);
      if (space > 0) return space + 1;
   }
   return limit;
}

// Each chunk is at most `limit` UTF-16 code units, which is how Discord counts.
function splitMessage(content: string, limit: number, mode: SplitMode): string[] {
   const chunks: string[] = [];
   let remaining = content;
   while (remaining.length > limit) {
      const at = findSplitIndex(remaining, limit, mode);
      chunks.push(remaining.slice(0, at));
      remaining = remaining.slice(at);
   }
   if (remaining) chunks.push(remaining);
   return chunks;
}

async function slowmodeOf(channelId: string): Promise<number> {
   const res = await phoenix.discord.get(`/channels/${channelId}`);
   if (!res.ok) return 0;
   return res.json<Phoenix.Channel>().rate_limit_per_user ?? 0;
}

async function sendChunks(channelId: string, chunks: string[], delayMs: number): Promise<number> {
   let sent = 0;
   try {
      for (const [index, content] of chunks.entries()) {
         const res = await phoenix.discord.post(`/channels/${channelId}/messages`, { content });
         if (!res.ok) throw new Error(`status ${res.status}`);
         sent++;
         if (index < chunks.length - 1) await phoenix.sleep(delayMs);
      }
   } catch (error) {
      phoenix.log.error(`Failed after sending ${sent}/${chunks.length} message parts.`, error);
   }
   return sent;
}

async function run(text: string, channelId: string, limit: number) {
   // Equicord skips the slowmode wait for members who can manage messages. Plugins cannot read
   // permissions, so everyone waits out the slowmode.
   const slowmode = await slowmodeOf(channelId);
   if (slowmode > 0 && (!settings.splitInSlowmode || slowmode > clamp(settings.slowmodeMax, 1, 30))) {
      phoenix.ui.toast("Cannot split this message because of the channel's slowmode.");
      phoenix.ui.copy(text);
      return;
   }
   const delayMs = Math.max(clamp(settings.sendDelay, 0.5, 10) * 1000, slowmode * 1000 + (slowmode ? SLOWMODE_BUFFER_MS : 0));
   const chunks = splitMessage(text, limit, settings.splitMode as SplitMode);
   const sent = await sendChunks(channelId, chunks, delayMs);
   if (sent !== chunks.length) {
      // The message box is already cleared and cannot be written to, so the rest goes to the clipboard.
      phoenix.ui.copy(chunks.slice(sent).join(""));
      phoenix.ui.toast(`Only ${sent}/${chunks.length} message parts were sent, the rest is copied.`);
   }
}

phoenix.messages.onSend((text, ctx) => {
   const limit = premium ? NITRO_MESSAGE_LIMIT : MESSAGE_LIMIT;
   if (text.length <= limit || !ctx.channelId) return;
   run(text, ctx.channelId, limit).catch((error) => phoenix.log.error("split failed", error));
   return false;
});
