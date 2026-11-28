const customEmoji = /<a?:[A-Za-z0-9_]+:[1-9]\d{0,19}>/g;

phoenix.ui.addMessageAction({
   label: "Copy Emoji Markdown",
   icon: "message.actions.copy_text",
   visible: ({ message }) => (message?.content?.match(customEmoji)?.length ?? 0) > 0,
   async onPress({ message }) {
      const options = [...new Set(message?.content?.match(customEmoji))];
      if (!options.length) return;

      const selected = options.length === 1 ? 0 : await phoenix.ui.choose({
         title: "Copy Emoji Markdown",
         options,
      });
      const emoji = options[selected];
      if (!emoji) return;

      phoenix.ui.copy(emoji);
      phoenix.ui.toast("Copied emoji markdown");
   },
});
