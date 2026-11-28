function profileId(message: Phoenix.Message | undefined): string | undefined {
   const id = message?.author?.id;
   if (id && id !== message?.webhook_id && /^[1-9]\d{0,19}$/.test(id)) return id;
}

phoenix.ui.addMessageAction({
   label: "Copy User URL",
   icon: "message.actions.copy_link",
   visible: ({ message }) => profileId(message) !== undefined,
   onPress({ message }) {
      const id = profileId(message);
      if (!id) return;

      phoenix.ui.copy(`<https://discord.com/users/${id}>`);
      phoenix.ui.toast("Copied user URL");
   },
});
