const ICON = "res://ic_mana_puzzle_piece_retry";

function repeatContent(msg: Phoenix.Message): string {
   const files = [
      ...(msg.attachments ?? []),
      ...(msg.message_snapshots ?? []).flatMap((s) => s.message.attachments ?? []),
   ];
   let content = msg.content;
   if (files.length) {
      content += "\nAttachments:";
      for (const file of files) content += `\n - ${file.filename} (${file.url})`;
   }
   return content;
}

async function repeat(msg: Phoenix.Message | undefined, reply: boolean, ctx: Phoenix.MessageContext) {
   if (!msg) {
      ctx.reply("Repeat: that message is not cached");
      return;
   }
   const body: Record<string, unknown> = {
      content: repeatContent(msg),
      allowed_mentions: { parse: [], replied_user: false },
      sticker_ids: (msg.sticker_items ?? []).map((s) => s.id),
   };
   if (reply) body.message_reference = { channel_id: msg.channel_id, message_id: msg.id };
   const res = await phoenix.discord.post(`/channels/${msg.channel_id}/messages`, body);
   if (!res.ok) phoenix.ui.toast(`Repeat failed (${res.status})`);
}

// Equicord's one row turns into Repeat and Reply with Shift held, and a touch screen has no Shift.
phoenix.ui.addMessageAction({
   label: "Repeat",
   icon: ICON,
   onPress: (ctx) => void repeat(ctx.message, false, ctx),
});

phoenix.ui.addMessageAction({
   label: "Repeat and Reply",
   icon: ICON,
   onPress: (ctx) => void repeat(ctx.message, true, ctx),
});
