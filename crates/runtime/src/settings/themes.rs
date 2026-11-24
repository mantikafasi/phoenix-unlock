//! The Themes page and per-theme detail page in Phoenix Unlocker settings.

use phoenix_ui::Prop;
use phoenix_wire::Value;

use crate::{
   settings::{
      Field, ICON_FILE, ICON_THEME, ICON_UNINSTALL, Press,
      console::log_line,
      rows::{InputKind, button_row, section, switch_row, text_field, value_row},
      tree::Tree,
   },
   themes::InstalledTheme,
};

pub fn themes_sections(status: Option<&(String, i64)>, themes: &[InstalledTheme]) -> Vec<Tree> {
   let rows = if themes.is_empty() {
      vec![button_row("No themes installed", ICON_THEME, None, false)]
   } else {
      themes
         .iter()
         .enumerate()
         .map(|(index, theme)| {
            let subtitle = theme.description.clone().or_else(|| {
               (!theme.authors.is_empty()).then(|| format!("By {}", theme.authors.join(", ")))
            });
            switch_row(
               Some(ICON_THEME),
               &theme.name,
               subtitle.as_deref(),
               theme.active,
               Press::OpenTheme(index),
               Press::ToggleTheme(index),
            )
         })
         .collect()
   };

   let mut sections = vec![section("Installed themes", rows)];

   let state = crate::themes::state();
   let has_active = state.colors.is_some()
      || state.fonts.is_some()
      || state.icons.is_some()
      || state.emoji.is_some();
   if has_active {
      sections.push(section(
         "Active",
         vec![
            value_row("Base theme", crate::themes::base().as_str()),
            button_row(
               "Clear active theme",
               ICON_UNINSTALL,
               Some(Press::ClearTheme),
               false,
            ),
         ],
      ));
   }

   sections.push(section(
      "From a URL",
      vec![text_field(
         Field::InstallTheme,
         Some(ICON_THEME),
         "https://example.com/theme.json",
         "",
         InputKind::Text,
      )],
   ));
   sections.push(section(
      "From a file",
      vec![button_row(
         "Choose a .json file",
         ICON_FILE,
         Some(Press::PickThemeFile),
         false,
      )],
   ));

   if let Some((message, color)) = status {
      let mut line = log_line(&[(message.clone(), *color)]);
      line.set(Prop::FONT_FAMILY, None);
      line.set(Prop::FONT_SIZE, Some(Value::F64(14.0)));
      line.set(Prop::LINE_HEIGHT, Some(Value::F64(18.0)));
      sections.push(section("", vec![line]));
   }

   sections
}

pub fn theme_detail_sections(index: usize, theme: &InstalledTheme) -> Vec<Tree> {
   let mut about = vec![value_row("Id", &theme.id)];
   if !theme.authors.is_empty() {
      about.push(value_row("Authors", &theme.authors.join(", ")));
   }
   if let Some(description) = &theme.description {
      about.push(value_row("Description", description));
   }
   vec![
      section(
         "Actions",
         vec![
            switch_row(
               Some(ICON_THEME),
               "Enabled",
               None,
               theme.active,
               Press::ToggleTheme(index),
               Press::ToggleTheme(index),
            ),
            button_row(
               "Uninstall",
               ICON_UNINSTALL,
               Some(Press::UninstallTheme(index)),
               false,
            ),
         ],
      ),
      section("About", about),
   ]
}
