const spotify = phoenix.spotify;
const held: Partial<Record<keyof Phoenix.Spotify, Phoenix.Remover>> = {};

const sync = (tweak: keyof Phoenix.Spotify, on: boolean) => {
  const hold = spotify?.[tweak];
  if (on && !held[tweak] && hold) {
    held[tweak] = hold();
  } else if (!on && held[tweak]) {
    held[tweak]();
    delete held[tweak];
  }
};

const settings = phoenix.settings.define({
  keepPlayingInVoice: {
    type: "switch",
    title: "No auto-pause",
    description: "Keep Spotify playing after 30 seconds of talking in voice",
    default: true,
    onChange: (on) => sync("keepPlayingInVoice", on),
  },
  keepActivityWhenIdle: {
    type: "switch",
    title: "Keep activity while idle",
    description: "Keep showing what you listen to when you go idle",
    default: true,
    onChange: (on) => sync("keepActivityWhenIdle", on),
  },
});

for (const tweak of ["keepPlayingInVoice", "keepActivityWhenIdle"] as const) {
  if (spotify?.[tweak]) sync(tweak, settings[tweak]);
  else phoenix.log.warn(`${tweak} is unavailable on this build of Phoenix`);
}
