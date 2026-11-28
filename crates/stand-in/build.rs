fn main() {
   // GNU ld omits the note unless asked. sigs keys its build table on it, as
   // for Phoenix. Only ELF has the note: Apple's ld64 rejects `--build-id` and
   // emits LC_UUID by default, and MSVC's link.exe rejects `-Wl,` flags.
   let unix = std::env::var("CARGO_CFG_UNIX").is_ok();
   if unix && std::env::var("CARGO_CFG_TARGET_VENDOR").as_deref() != Ok("apple") {
      println!("cargo:rustc-cdylib-link-arg=-Wl,--build-id=sha1");
   }
}
