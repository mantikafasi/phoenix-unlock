//! The runtime calls behind presses, submits and picked files.

use std::sync::atomic::Ordering;

use crate::{
   Chosen, Control, PluginInfo,
   settings::{
      COLOR_ERROR, COLOR_STRING, Field, Job, LOG_QUEUED, PickTarget, Press, TITLE, settings,
   },
   themes::InstalledTheme,
   update::Catalog,
};

fn plugin(index: usize) -> Option<PluginInfo> {
   settings().plugins.get(index).cloned()
}

fn theme(index: usize) -> Option<InstalledTheme> {
   settings().themes.get(index).cloned()
}

/// Makes the runtime calls `job` asks for. They take the engine lock, so they
/// run before the settings lock is taken.
pub fn act(job: &Job) {
   match *job {
      Job::Log => LOG_QUEUED.store(false, Ordering::Relaxed),
      // The editor is multiline: Enter is a newline there, and Save saves.
      Job::Submit(Field::Code(_)) | Job::NavigateTo(_) | Job::Draft(..) | Job::Refresh => {},
      Job::Submit(field) => submit(field),
      Job::Picked(ref picked) => {
         let target = settings().pick_target;
         match (target, picked) {
            (PickTarget::Plugin, Ok(Some((file, bytes)))) => {
               installed(crate::install_file(file, bytes.clone()), file);
            },
            (PickTarget::Theme, Ok(Some((file, bytes)))) => {
               theme_installed(crate::themes::install_file(file, bytes), file);
            },
            (_, Ok(None)) => {},
            (PickTarget::Plugin, Err(err)) => {
               log::error!("file picker: {err}");
               settings().install_status =
                  Some((format!("The file picker failed: {err}"), COLOR_ERROR));
            },
            (PickTarget::Theme, Err(err)) => {
               log::error!("file picker: {err}");
               settings().theme_status =
                  Some((format!("The file picker failed: {err}"), COLOR_ERROR));
            },
         }
      },
      Job::Press(pressed) => press(pressed),
   }
}

fn submit(field: Field) {
   let Some(draft) = settings().drafts.remove(&field) else {
      return;
   };
   let input = draft.trim();
   match field {
      Field::Eval if !input.is_empty() => crate::evaluate(input),
      Field::Install if !input.is_empty() => {
         installed(crate::install(input), input);
      },
      Field::InstallTheme if !input.is_empty() => {
         theme_installed(crate::themes::install_url(input), input);
      },
      Field::Setting(index, at) => {
         let Some(plugin) = plugin(index) else {
            return;
         };
         let Some(setting) = plugin.settings.get(at) else {
            return;
         };
         let value = match setting.control {
            Control::Text(_) | Control::Secret(_) => Chosen::Text(draft.clone()),
            Control::Number(_) => {
               let Ok(number) = input.parse::<f64>() else {
                  log::error!(
                     "[{}] {}: {input:?} is not a number",
                     plugin.name,
                     setting.key
                  );
                  return;
               };
               Chosen::Number(number)
            },
            Control::Switch(_) | Control::Select { .. } | Control::Button => return,
         };
         crate::set_setting(&plugin.name, &setting.key, value);
      },
      Field::Eval | Field::Install | Field::InstallTheme | Field::Code(_) => {},
   }
}

fn press(pressed: Press) {
   match pressed {
      Press::PickFile => {
         settings().pick_target = PickTarget::Plugin;
         if let Err(err) = crate::picker::pick() {
            log::error!("file picker: {err}");
            settings().install_status =
               Some((format!("The file picker failed: {err}"), COLOR_ERROR));
         }
      },
      Press::PickThemeFile => {
         settings().pick_target = PickTarget::Theme;
         if let Err(err) = crate::picker::pick() {
            log::error!("file picker: {err}");
            settings().theme_status = Some((format!("The file picker failed: {err}"), COLOR_ERROR));
         }
      },
      Press::ToggleTheme(index) => {
         if let Some(theme) = theme(index) {
            if theme.active {
               crate::themes::set_colors(None);
               settings().theme_status = Some((format!("Disabled {}.", theme.name), COLOR_STRING));
            } else {
               match crate::themes::apply_installed(&theme.id) {
                  Ok(()) => {
                     settings().theme_status =
                        Some((format!("Applied {}.", theme.name), COLOR_STRING));
                  },
                  Err(err) => {
                     log::error!("applying theme {} failed: {err}", theme.id);
                     settings().theme_status = Some((
                        format!("Failed to apply {}: {err}", theme.name),
                        COLOR_ERROR,
                     ));
                  },
               }
            }
         }
      },
      Press::UninstallTheme(index) => {
         if let Some(theme) = theme(index) {
            match crate::themes::uninstall(&theme.id) {
               Ok(()) => {
                  log::info!("uninstalled theme {}", theme.id);
                  settings().theme_status =
                     Some((format!("Uninstalled {}.", theme.name), COLOR_STRING));
               },
               Err(err) => {
                  log::error!("uninstalling theme {} failed: {err}", theme.id);
                  settings().theme_status = Some((format!("Uninstall failed: {err}"), COLOR_ERROR));
               },
            }
         }
      },
      Press::ClearTheme => {
         crate::themes::clear_all();
         settings().theme_status = Some(("Cleared active theme.".to_owned(), COLOR_STRING));
      },
      // The editor starts from the plugin's code, kept as the field's draft.
      Press::EditCode(index) => {
         if let Some(plugin) = plugin(index)
            && let Some(source) = crate::source(&plugin.name)
         {
            settings().drafts.insert(Field::Code(index), source);
         }
      },
      Press::SaveCode(index) => {
         let draft = settings().drafts.get(&Field::Code(index)).cloned();
         if let (Some(plugin), Some(source)) = (plugin(index), draft) {
            match crate::save_source(&plugin.name, source) {
               Ok(()) => log::info!("saved {} and restarted it", plugin.name),
               Err(err) => log::error!("saving {} failed: {err}", plugin.name),
            }
         }
      },
      Press::Toggle(index) => {
         if let Some(plugin) = plugin(index) {
            crate::set_enabled(&plugin.name, !plugin.enabled);
         }
      },
      Press::Reload(index) => {
         if let Some(plugin) = plugin(index) {
            crate::reload(&plugin.name);
         }
      },
      Press::Uninstall(index) => {
         if let Some(plugin) = plugin(index) {
            match crate::uninstall(&plugin.name) {
               Ok(()) => log::info!("uninstalled {}", plugin.name),
               Err(err) => log::error!("uninstall failed: {err}"),
            }
         }
      },
      Press::Setting(index, at) => {
         let Some(plugin) = plugin(index) else {
            return;
         };
         match plugin
            .settings
            .get(at)
            .map(|setting| (&setting.key, &setting.control))
         {
            Some((key, Control::Switch(on))) => {
               crate::set_setting(&plugin.name, key, Chosen::Switch(!on));
            },
            Some((key, Control::Button)) => crate::press(&plugin.name, key),
            _ => {},
         }
      },
      Press::Choose(index, at, option) => {
         let Some(plugin) = plugin(index) else {
            return;
         };
         if let Some(setting) = plugin.settings.get(at)
            && let Control::Select { options, .. } = &setting.control
            && let Some(choice) = options.get(option)
         {
            crate::set_setting(
               &plugin.name,
               &setting.key,
               Chosen::Text(choice.value.clone()),
            );
         }
      },
      Press::Listed(index) => listed(index),
      Press::ApplyUpdate => {
         crate::update::apply();
      },
      Press::OpenSite => {
         if let Err(err) = crate::picker::open(crate::update::SITE) {
            log::error!("opening {}: {err}", crate::update::SITE);
         }
      },
      Press::ToggleStaff => {
         crate::set_staff_enabled(!crate::staff_enabled());
      },
      Press::ToggleRemoteShell => {
         crate::set_remote_shell_enabled(!crate::remote_shell_enabled());
      },
      Press::OpenInstall => crate::update::fetch_catalog(),
      Press::OpenMain
      | Press::OpenConsole
      | Press::OpenThemes
      | Press::OpenTheme(_)
      | Press::OpenPlugin(_)
      | Press::Back => {},
   }
}

/// Flips a listed plugin's switch. An absent plugin installs, an installed one
/// uninstalls, and one bundled in the APK, which cannot go, turns on or off.
fn listed(index: usize) {
   let Catalog::Loaded(listed) = crate::update::catalog() else {
      return;
   };
   let Some(plugin) = listed.get(index) else {
      return;
   };
   let present = settings()
      .plugins
      .iter()
      .find(|other| other.name == plugin.name)
      .cloned();
   match present {
      None => installed(crate::update::install_listed(&plugin.name), &plugin.name),
      Some(info) if !info.installed => crate::set_enabled(&info.name, !info.enabled),
      Some(info) => {
         settings().install_status = Some(match crate::uninstall(&info.name) {
            Ok(()) => (format!("Uninstalled {}.", info.name), COLOR_STRING),
            Err(err) => (format!("Uninstall failed: {err}"), COLOR_ERROR),
         });
      },
   }
}

/// Logs an install's outcome and keeps it for the install page.
fn installed(result: Result<String, crate::Error>, from: &str) {
   let status = match result {
      Ok(name) => {
         log::info!("installed {name} from {from}");
         (
            format!("Installed {name}. It is on the {TITLE} page now."),
            COLOR_STRING,
         )
      },
      Err(err) => {
         log::error!("install failed: {err}");
         (format!("Install failed: {err}"), COLOR_ERROR)
      },
   };
   settings().install_status = Some(status);
}

fn theme_installed(result: Result<String, crate::Error>, from: &str) {
   let status = match result {
      Ok(name) => {
         log::info!("installed theme {name} from {from}");
         (format!("Installed and applied {name}."), COLOR_STRING)
      },
      Err(err) => {
         log::error!("theme install failed: {err}");
         (format!("Theme install failed: {err}"), COLOR_ERROR)
      },
   };
   settings().theme_status = Some(status);
}
