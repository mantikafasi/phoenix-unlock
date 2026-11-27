// Equicord adds a row to the user context menu. Phoenix has no menu hook for users, so the
// button sits on a profile tab of its own.
phoenix.ui.addProfileTab({
   title: "Mention",
   render({ userId }) {
      if (!userId) return [{ type: "text", text: "This profile has no user id to mention." }];
      return [
         {
            type: "button",
            label: "Copy User Mention",
            onPress() {
               phoenix.ui.copy(`<@${userId}>`);
               phoenix.ui.toast("Copied user mention");
            },
         },
      ];
   },
});
