//! `phoenix.commands` in Discord's own `/` list. Desktop draws that list from
//! the application command index, so plugin commands join the index as the
//! commands of a Sunbird app, and the interaction Discord sends for one runs
//! the plugin instead of reaching Discord.

use std::{
   sync::{
      Mutex,
      PoisonError,
   },
   time::{
      SystemTime,
      UNIX_EPOCH,
   },
};

use phoenix_runtime::discord::{
   InFlight,
   RequestAction,
   api_path,
   watch_rewritten,
};
use serde::{
   Deserialize,
   Serialize,
};
use serde_json::value::RawValue;

use crate::{
   gateway_send,
   js,
};

/// The Sunbird app's id. Discord has no app with it, and the index only needs
/// a snowflake.
const APP_ID: &str = "1";

/// A plugin command as the index lists it.
#[derive(Clone)]
pub struct Entry {
   pub name:        String,
   pub description: String,
   pub plugin:      String,
}

static ENTRIES: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

pub fn sync(entries: Vec<Entry>) {
   *ENTRIES.lock().unwrap_or_else(PoisonError::into_inner) = entries;
}

pub fn any() -> bool {
   !ENTRIES
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .is_empty()
}

pub fn entries() -> Vec<Entry> {
   ENTRIES
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .clone()
}

/// The index response to `method url`, when the plugin commands belong in it.
pub fn watch(method: &str, url: &str) -> Option<InFlight> {
   let path = api_path(url)?;
   let index = method.eq_ignore_ascii_case("GET")
      && path
         .split('?')
         .next()
         .is_some_and(|path| path.ends_with("/application-command-index"));
   if !index || !any() {
      return None;
   }
   watch_rewritten(method, url, add_entries)
}

#[derive(Deserialize, Serialize)]
struct Index {
   applications:         Vec<Box<RawValue>>,
   application_commands: Vec<Box<RawValue>>,
   #[serde(flatten)]
   rest:                 serde_json::Map<String, serde_json::Value>,
}

#[derive(Serialize)]
struct App {
   id:          &'static str,
   name:        &'static str,
   description: &'static str,
   icon:        Option<&'static str>,
   flags:       &'static str,
}

#[derive(Serialize)]
struct Command<'a> {
   id:                         String,
   #[serde(rename = "type")]
   kind:                       u8,
   application_id:             &'static str,
   version:                    &'static str,
   name:                       &'a str,
   description:                &'a str,
   default_member_permissions: Option<&'static str>,
   dm_permission:              bool,
   integration_types:          [u8; 2],
   contexts:                   [u8; 3],
   options:                    [CommandOption; 1],
}

/// The one string a plugin's `execute` takes, as Discord's option.
#[derive(Serialize)]
struct CommandOption {
   #[serde(rename = "type")]
   kind:        u8,
   name:        &'static str,
   description: &'static str,
   required:    bool,
}

const CHAT_INPUT: u8 = 1;
const STRING: u8 = 3;
const ARGS: &str = "args";

fn add_entries(body: Vec<u8>) -> Vec<u8> {
   let Ok(mut index) = serde_json::from_slice::<Index>(&body) else {
      return body;
   };
   let app = App {
      id:          APP_ID,
      name:        "Sunbird",
      description: "",
      icon:        None,
      flags:       "0",
   };
   let entries = ENTRIES.lock().unwrap_or_else(PoisonError::into_inner);
   let commands = entries.iter().enumerate().map(|(at, entry)| {
      Command {
         id:                         (at + 2).to_string(),
         kind:                       CHAT_INPUT,
         application_id:             APP_ID,
         version:                    "1",
         name:                       &entry.name,
         description:                if entry.description.is_empty() {
            &entry.plugin
         } else {
            &entry.description
         },
         default_member_permissions: None,
         dm_permission:              true,
         integration_types:          [0, 1],
         contexts:                   [0, 1, 2],
         options:                    [CommandOption {
            kind:        STRING,
            name:        ARGS,
            description: "Arguments",
            required:    false,
         }],
      }
   });
   let added = commands
      .map(|command| serde_json::value::to_raw_value(&command))
      .collect::<Result<Vec<_>, _>>();
   drop(entries);
   let (Ok(app), Ok(mut added)) = (serde_json::value::to_raw_value(&app), added) else {
      return body;
   };
   index.applications.push(app);
   index.application_commands.append(&mut added);
   serde_json::to_vec(&index).unwrap_or(body)
}

#[derive(Deserialize)]
struct Interaction {
   application_id: String,
   channel_id:     String,
   nonce:          Option<String>,
   data:           InteractionData,
}

#[derive(Deserialize)]
struct InteractionData {
   name:    String,
   #[serde(default)]
   options: Vec<InteractionOption>,
}

#[derive(Deserialize)]
struct InteractionOption {
   name:  String,
   value: String,
}

/// What Discord would hear from an app that answered at once, so the client
/// clears the pending command.
#[derive(Serialize)]
struct Answered<'a> {
   id:    String,
   nonce: &'a str,
}

/// Runs the plugin command an interaction names and keeps the interaction from
/// Discord, which has no Sunbird app to deliver it to.
pub fn intercept(method: &str, path: &str, body: &[u8]) -> Option<RequestAction> {
   if !method.eq_ignore_ascii_case("POST") || path.split('?').next() != Some("/interactions") {
      return None;
   }
   let interaction = serde_json::from_slice::<Interaction>(body).ok()?;
   if interaction.application_id != APP_ID {
      return None;
   }
   if let Some(nonce) = &interaction.nonce {
      answer(nonce);
   }
   let args = interaction
      .data
      .options
      .into_iter()
      .find(|option| option.name == ARGS)
      .map_or_default(|option| option.value);
   let name = interaction.data.name;
   let channel = interaction.channel_id;
   std::thread::spawn(move || js::commands::run_and_send(&name, &args, &channel));
   Some(RequestAction::Drop)
}

fn answer(nonce: &str) {
   const DISCORD_EPOCH_MS: u128 = 1_420_070_400_000;

   let now = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .map_or(0, |since| since.as_millis());
   let id = (now.saturating_sub(DISCORD_EPOCH_MS) << 22).to_string();
   let answered = Answered { id, nonce };
   for event in ["INTERACTION_CREATE", "INTERACTION_SUCCESS"] {
      gateway_send::inject(event, &answered);
   }
}
