// @description Composer toggle: while on, everything you send is wrapped in spoiler tags
// @version 1.0.0
// @author cmdr-chara

const KEY = "enabled";
const LIMIT = 2000;

let on = phoenix.storage.get<boolean>(KEY) ?? false;

phoenix.ui.addComposerButton({
  icon: "res://ic_mana_image_spoiler",
  label: "Spoiler mode",
  onPress() {
    on = !on;
    phoenix.storage.set(KEY, on);
    phoenix.ui.toast(on ? "Spoiler mode on" : "Spoiler mode off");
  },
});

phoenix.messages.onSend((text) => {
  if (!on || !text.trim()) return;
  if (text.includes("||")) {
    // The draft already has spoiler tags; wrapping it would un-hide text.
    return;
  }
  if (text.length + 4 > LIMIT) {
    phoenix.ui.toast("Message too long to wrap in spoilers");
    return;
  }
  return `||${text}||`;
});
