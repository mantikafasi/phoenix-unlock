//! Shared Sunbird theme parser, semantic token mapper, and persistence layer.
//!
//! Platform-agnostic core consumed by desktop (`ManaTheme` semantic engine),
//! mobile, and the theme catalog / Settings UI.

pub mod aliases;
pub mod color;
pub mod css;
pub mod manifest;
pub mod spec;
pub mod store;

pub use color::{
   Color,
   Hsla,
   parse_css_color,
};
pub use css::{
   ThemeMeta,
   ThemeMode,
};
pub use manifest::ThemeManifest;
pub use spec::{
   ResolvedPalette,
   ThemeSpec,
};
pub use store::{
   InstalledTheme,
   ThemeConfig,
   list_themes,
   load_active_spec,
   load_config,
   read_contained_theme,
   save_config,
   valid_theme_file,
};
