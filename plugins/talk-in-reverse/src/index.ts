const ICON = "res://ic_mana_puzzle_piece_retry";
const GREEN = 0xff23a55a;

// Like Equicord, the toggle is not saved: it starts off every time the app starts.
let on = false;

const label = () => (on ? "Disable Reverse Message" : "Enable Reverse Message");

const button = phoenix.ui.addComposerButton({
   icon: ICON,
   label: label(),
   onPress() {
      on = !on;
      button.update({ icon: ICON, label: label(), tint: on ? GREEN : null });
      phoenix.ui.toast(on ? "Reverse message on" : "Reverse message off");
   },
});

phoenix.messages.onSend((text) => {
   if (!on || !text) return;
   // Spread by code point, so emoji and other astral characters are not split into broken halves.
   return [...text].reverse().join("");
});
