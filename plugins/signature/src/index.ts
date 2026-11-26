const ICON = "res://ic_mana_angle_brackets";
const GREEN = 0xff23a55a;
const RED = 0xfff23f43;

const settings = phoenix.settings.define({
   name: {
      type: "text",
      title: "Signature",
      description: "The signature added to the end of your messages",
      default: "a chronic discord user",
   },
   textHeader: {
      type: "text",
      title: "Header",
      description: "What to put in front of the signature, like > for a quote",
      default: ">",
   },
   showIcon: {
      type: "switch",
      title: "Show composer button",
      description: "A button to toggle the signature in the message box",
      default: true,
      onChange: () => syncButton(),
   },
   isEnabled: {
      type: "switch",
      title: "Enabled",
      description: "Toggle functionality",
      default: true,
      onChange: (value) => setEnabled(value),
   },
});

// The button and /signature flip this without being able to move the switch, so the last one
// touched wins, as in spoiler-mode.
let enabled = phoenix.storage.get<boolean>("enabled") ?? settings.isEnabled;
let button: Phoenix.ButtonRemover | null = null;

const label = () => (enabled ? "Disable Signature" : "Enable Signature");

function syncButton() {
   if (!settings.showIcon) {
      button?.();
      button = null;
      return;
   }
   const patch = { icon: ICON, label: label(), tint: enabled ? GREEN : RED };
   if (button) {
      button.update(patch);
      return;
   }
   button = phoenix.ui.addComposerButton({
      ...patch,
      onPress() {
         setEnabled(!enabled);
         phoenix.ui.toast(enabled ? "Signature enabled" : "Signature disabled");
      },
   });
}

function setEnabled(value: boolean) {
   enabled = value;
   phoenix.storage.set("enabled", value);
   syncButton();
}
syncButton();

const ON = new Set(["true", "on", "yes", "1", "enable"]);
const OFF = new Set(["false", "off", "no", "0", "disable"]);

phoenix.commands.register({
   name: "signature",
   description: "Toggle your signature (default is toggle)",
   execute(args, ctx) {
      const word = args.trim().toLowerCase();
      setEnabled(ON.has(word) ? true : OFF.has(word) ? false : !enabled);
      ctx.reply(enabled ? "Signature enabled!" : "Signature disabled!");
   },
});

phoenix.messages.onSend((text) => {
   if (!enabled || !text) return;
   const header = settings.textHeader ? `${settings.textHeader} ` : "";
   return `${text}\n${header}${settings.name}`;
});
