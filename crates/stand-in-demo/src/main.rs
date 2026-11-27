//! End-to-end demo against the stand-in.
//!
//! `stand-in-demo` builds the stand-in and the loader in release, bundles the
//! example plugin with esbuild, then runs the scenario in `demo.rs` in this process.
//!
//! `stand-in-demo run <libstand_in.so> <libphoenix.so> <home>` runs the
//! scenario alone, which is how it runs on a phone over adb. It loads the
//! stand-in the way Android loads Phoenix, loads the loader after it the way a
//! `DT_NEEDED` entry would, and feeds the stand-in two gateway frames.

#[cfg(unix)]
mod demo;

#[cfg(unix)]
fn main() -> misstep::Result<()> {
   demo::main()
}

#[cfg(not(unix))]
fn main() -> misstep::Result<()> {
   misstep::bail!("stand-in-demo loads libraries with dlopen, so it only runs on unix")
}
