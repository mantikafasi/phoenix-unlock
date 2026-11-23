// @description Shows a message's raw JSON in a local reply
// @version 1.0.0
// @author cmdr-chara

const LIMIT = 1800;

const dump = (value: unknown) => {
  // Zero-width spaces keep backticks inside the JSON from closing the block.
  const json = (JSON.stringify(value, null, 2) ?? "null").replace(/```/g, "`\u200b`\u200b`");
  return json.length > LIMIT ? `${json.slice(0, LIMIT)}\n… truncated` : json;
};

phoenix.ui.addMessageAction({
  label: "View raw",
  icon: "res://ic_mana_angle_brackets",
  onPress(ctx) {
    if (!ctx.message) {
      ctx.reply("this message is not cached, nothing to show");
      return;
    }
    ctx.reply(`\`\`\`json\n${dump(ctx.message)}\n\`\`\``);
  },
});
