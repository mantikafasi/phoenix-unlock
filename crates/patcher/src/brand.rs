//! Sunbird launcher and in-app brand resources replacing Discord's marks while
//! patching.

pub fn replacement(name: &str) -> Option<&'static [u8]> {
   match name {
      "res/mipmap-xhdpi-v4/ic_launcher_foreground.png"
      | "res/mipmap-xhdpi-v4/ic_logo_foreground.png" => {
         Some(include_bytes!("../brand/foreground-216.png"))
      },
      "res/mipmap-xxhdpi-v4/ic_launcher_foreground.png"
      | "res/mipmap-xxhdpi-v4/ic_logo_foreground.png" => {
         Some(include_bytes!("../brand/foreground-324.png"))
      },
      "res/mipmap-xxxhdpi-v4/ic_launcher_foreground.png"
      | "res/mipmap-xxxhdpi-v4/ic_logo_foreground.png" => {
         Some(include_bytes!("../brand/foreground-432.png"))
      },
      "res/mipmap-xhdpi-v4/ic_logo_round_canary.png" => {
         Some(include_bytes!("../brand/round-96.png"))
      },
      "res/mipmap-xxhdpi-v4/ic_logo_round_canary.png" => {
         Some(include_bytes!("../brand/round-144.png"))
      },
      "res/mipmap-xxxhdpi-v4/ic_logo_round_canary.png" => {
         Some(include_bytes!("../brand/round-192.png"))
      },
      "res/mipmap-xhdpi-v4/ic_logo_square_canary.png" => {
         Some(include_bytes!("../brand/square-96.png"))
      },
      "res/mipmap-xxhdpi-v4/ic_logo_square_canary.png" => {
         Some(include_bytes!("../brand/square-144.png"))
      },
      "res/mipmap-xxxhdpi-v4/ic_logo_square_canary.png" => {
         Some(include_bytes!("../brand/square-192.png"))
      },
      "res/mipmap-xxhdpi-v4/img_logo_dark.png" => {
         Some(include_bytes!("../brand/img_logo_dark.png"))
      },
      "res/mipmap-xxhdpi-v4/img_logo_light.png" => {
         Some(include_bytes!("../brand/img_logo_light.png"))
      },
      "res/drawable-xxhdpi-v4/images_native_logo.png" => {
         Some(include_bytes!("../brand/images_native_logo.png"))
      },
      _ => None,
   }
}

/// Replaces Phoenix's gold launcher background with Sunbird red in a merged
/// resource table.
pub fn arsc(data: &mut [u8]) {
   let gold = 0xFFF0_B232u32.to_le_bytes();
   let red = 0xFFEC_4144u32.to_le_bytes();
   if let Some(at) = data.windows(4).position(|bytes| bytes == gold) {
      data[at..at + 4].copy_from_slice(&red);
   }
}
