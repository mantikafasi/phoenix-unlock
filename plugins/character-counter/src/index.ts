const AID = "phoenix-character-counter";
const NITRO_MAX = 4000;
const BASE_MAX = 2000;
const MUTED = 0xff949ba4;
const YELLOW = 0xfff0b232;
const ORANGE = 0xffe8731a;
const RED = 0xfff23f43;
const PLAIN = 0xffb5bac1;

const settings = phoenix.settings.define({
   colorEffects: {
      type: "switch",
      title: "Color effects",
      description: "Turn yellow, orange and red as you get closer to the character limit",
      default: true,
      onChange: () => draw(),
   },
});

let max = BASE_MAX;
let length = 0;
let unmount: (() => void) | null = null;
let mountedOn: number | null = null;

const colorFor = (percent: number): number => {
   if (!settings.colorEffects) return PLAIN;
   if (percent < 50) return MUTED;
   if (percent < 75) return YELLOW;
   if (percent < 90) return ORANGE;
   return RED;
};

// The counter is a text node that stays mounted and is only rewritten, since mounting again on
// every keystroke would rebuild the row.
function draw() {
   for (const id of phoenix.ui.find(AID)) {
      phoenix.ui.update(id, {
         text: `${length}/${max}`,
         color: colorFor((length / max) * 100),
         hidden: length === 0,
      });
   }
}

function attach(composer: number) {
   if (mountedOn === composer) return;
   unmount?.();
   unmount = null;
   mountedOn = null;
   const parent = phoenix.ui.node(composer)?.parent;
   if (parent == null) return;
   unmount = phoenix.ui.mount(parent, {
      kind: "text",
      props: {
         accessibilityId: AID,
         text: "",
         fontSize: 11,
         color: MUTED,
         hidden: true,
         padding: [4, 0, 4, 0],
      },
   });
   mountedOn = composer;
   draw();
}

// A composer is created again when the user changes channel, so follow it each time.
phoenix.ui.onNode("composer", (node) => attach(node.id));
for (const id of phoenix.ui.find("composer")) attach(id);

// The text a user typed arrives as `changeText`. Sending clears the box and arrives the same way.
phoenix.ui.onEvent("changeText", (ev) => {
   if (ev.accessibilityId !== "composer") return;
   const text = (ev.payload as { text?: string } | null)?.text ?? "";
   length = [...text].length;
   draw();
});

// Nitro Max users get 4000 characters. Equicord read it off the live editor, which Phoenix
// does not expose, so it comes from the account's premium type instead.
phoenix.discord
   .me()
   .then((me) => {
      max = me?.premium_type === 2 ? NITRO_MAX : BASE_MAX;
      draw();
   })
   .catch(() => {});

phoenix.onUnload(() => {
   unmount?.();
   unmount = null;
});
