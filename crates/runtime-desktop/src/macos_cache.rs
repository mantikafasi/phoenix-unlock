//! `LC_UUID`-keyed cache for macOS arm64 startup data (`GatewayRegistry`).
//!
//! Stores RVAs relative to `__TEXT` base in `$PHOENIX_HOME/macos-cache.json`
//! (default `~/Library/Application Support/libdiscord/sunbird/macos-cache.json`)
//! keyed by `image.build_id`. Any Discord update changes `LC_UUID` and
//! invalidates the cache automatically.

#![cfg(all(target_os = "macos", target_arch = "aarch64"))]

use std::{
   env,
   fs,
   path::PathBuf,
};

use phoenix_hook::image::Image;
use phoenix_sigs::registry::{
   GatewayEntry,
   GatewayRegistry,
};

const CACHE_VERSION: u32 = 1;

#[derive(serde::Serialize, serde::Deserialize)]
struct CachedGatewayEntry {
   name:        String,
   slot_rvas:   Vec<usize>,
   handler_rva: usize,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct MacosCache {
   version:  u32,
   build_id: String,
   gateway:  Vec<CachedGatewayEntry>,
}

fn cache_path() -> PathBuf {
   if let Some(home) = env::var_os("PHOENIX_HOME") {
      return PathBuf::from(home).join("macos-cache.json");
   }
   let data = env::var_os("LIBDISCORD_DATA_DIR").map_or_else(
      || {
         env::var_os("HOME")
            .map_or_else(env::temp_dir, PathBuf::from)
            .join("Library/Application Support/libdiscord")
      },
      PathBuf::from,
   );
   data.join("sunbird").join("macos-cache.json")
}

fn image_base(image: &Image<'_>) -> Option<usize> {
   image.segments.iter().map(|seg| seg.addr).min()
}

/// Loads a cached [`GatewayRegistry`] for `image` if `LC_UUID` matches and all
/// cached slots and handlers validate in the current mapping.
pub fn load_gateway(image: &Image<'_>) -> Option<GatewayRegistry> {
   let build_id = image.build_id?.to_string();
   let base = image_base(image)?;
   let bytes = fs::read(cache_path()).ok()?;
   let cache: MacosCache = serde_json::from_slice(&bytes).ok()?;
   if cache.version != CACHE_VERSION || cache.build_id != build_id || cache.gateway.is_empty() {
      return None;
   }
   let mut entries = Vec::with_capacity(cache.gateway.len());
   for entry in cache.gateway {
      let handler = base.checked_add(entry.handler_rva)?;
      if !image.is_code(handler) || entry.slot_rvas.is_empty() {
         return None;
      }
      let mut slots = Vec::with_capacity(entry.slot_rvas.len());
      for rva in entry.slot_rvas {
         let slot = base.checked_add(rva)?;
         // Validate `{name_ptr, name_len, handler}` still matches this entry.
         let name_ptr = image.read_usize(slot)?;
         let name_len = image.read_usize(slot + size_of::<usize>())?;
         if name_len != entry.name.len() || image.read(name_ptr, name_len) != Some(entry.name.as_bytes()) {
            return None;
         }
         slots.push(slot);
      }
      entries.push(GatewayEntry {
         name: entry.name,
         slots,
         handler,
      });
   }
   log::info!(
      "macos cache: hit for {} ({} gateway events)",
      build_id,
      entries.len()
   );
   Some(GatewayRegistry::from_entries(entries))
}

/// Persists `registry` RVAs keyed by `image.build_id` for future warm starts.
pub fn save_gateway(image: &Image<'_>, registry: &GatewayRegistry) {
   let (Some(build_id), Some(base)) = (image.build_id, image_base(image)) else {
      return;
   };
   let mut gateway = Vec::new();
   for (_, entry) in registry.iter() {
      let Some(handler_rva) = entry.handler.checked_sub(base) else {
         return;
      };
      let Some(slot_rvas) = entry
         .slots
         .iter()
         .map(|&slot| slot.checked_sub(base))
         .collect::<Option<Vec<_>>>()
      else {
         return;
      };
      gateway.push(CachedGatewayEntry {
         name: entry.name.clone(),
         slot_rvas,
         handler_rva,
      });
   }
   let cache = MacosCache {
      version: CACHE_VERSION,
      build_id: build_id.to_string(),
      gateway,
   };
   let path = cache_path();
   if let Some(parent) = path.parent() {
      let _ = fs::create_dir_all(parent);
   }
   if let Ok(json) = serde_json::to_vec(&cache) {
      let tmp = path.with_extension("json.tmp");
      if fs::write(&tmp, &json).is_ok() {
         let _ = fs::rename(&tmp, &path);
      }
   }
}
