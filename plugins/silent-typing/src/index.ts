// @description Prevents Discord from sending typing indicators
// @version 1.0.1
// @author mantikafasi
// @discord POST /channels/*/typing

phoenix.discord.onRequest(
  { method: "POST", pathPrefix: "/channels/", pathSuffix: "/typing" },
  () => false,
);
