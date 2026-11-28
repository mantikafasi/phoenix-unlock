type Timer = "dmCall" | "guildAfk";

const held = new Map<Timer, Phoenix.Remover>();

const sync = (timer: Timer, off: boolean) => {
  const restore = held.get(timer);
  if (off && !restore && phoenix.ui.disableCallIdle) {
    held.set(timer, phoenix.ui.disableCallIdle(timer));
  } else if (!off && restore) {
    restore();
    held.delete(timer);
  }
};

const settings = phoenix.settings.define({
  dmCall: {
    type: "switch",
    title: "Stay in DM calls alone",
    description: "Discord leaves a DM call after 3 minutes with nobody else in it",
    default: true,
    onChange: (on) => sync("dmCall", on),
  },
  guildAfk: {
    type: "switch",
    title: "Skip the server AFK timeout",
    description: "Discord moves idle members to the AFK channel or disconnects them",
    default: true,
    onChange: (on) => sync("guildAfk", on),
  },
});

if (!phoenix.ui.disableCallIdle) phoenix.log.warn("call idle timers need Desktop Phoenix");
sync("dmCall", settings.dmCall);
sync("guildAfk", settings.guildAfk);
