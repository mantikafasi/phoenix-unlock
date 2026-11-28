//! What a plugin's `// @permissions` line lets it reach, like
//! `// @permissions gateway discord`. A plugin's `phoenix` object leaves out
//! what it did not declare.
//!
//! Images Discord draws reach the network too, so an icon a plugin draws, and
//! a URL host a hook adds to what Discord renders, must be Discord's own unless
//! the plugin holds `http`.

use std::{
   collections::{
      BTreeMap,
      BTreeSet,
   },
   fmt,
   str::FromStr,
};

use serde::{
   Deserializer,
   de::{
      DeserializeSeed,
      MapAccess,
      SeqAccess,
      Visitor,
   },
};

use crate::banner;

#[derive(Debug, flub::Error)]
pub enum PermissionError {
   #[error("@permissions: {0} is not one of gateway, discord, http, messages, hooks")]
   Unknown(String),
}

/// One thing a plugin reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Permission {
   /// Every gateway event and the message cache.
   Gateway,
   /// Any `phoenix.discord` request or interceptor, made as the user.
   Discord,
   /// `phoenix.fetch` to any host, and images and links from any host.
   Http,
   /// Reading and rewriting message and composer text.
   Messages,
   /// Host modules, the raw view tree and outgoing gateway frames. Implies the
   /// rest.
   Hooks,
}

impl Permission {
   #[must_use]
   pub const fn label(self) -> &'static str {
      match self {
         Self::Gateway => "Gateway events",
         Self::Discord => "Discord API",
         Self::Http => "Web requests",
         Self::Messages => "Messages",
         Self::Hooks => "Hooks",
      }
   }

   #[must_use]
   pub const fn summary(self) -> &'static str {
      match self {
         Self::Gateway => "every event and the message cache",
         Self::Discord => "any request as you",
         Self::Http => "any host",
         Self::Messages => "read and rewrite",
         Self::Hooks => "full access",
      }
   }
}

impl fmt::Display for Permission {
   fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
      formatter.write_str(match self {
         Self::Gateway => "gateway",
         Self::Discord => "discord",
         Self::Http => "http",
         Self::Messages => "messages",
         Self::Hooks => "hooks",
      })
   }
}

impl FromStr for Permission {
   type Err = PermissionError;

   fn from_str(name: &str) -> Result<Self, Self::Err> {
      match name {
         "gateway" => Ok(Self::Gateway),
         "discord" => Ok(Self::Discord),
         "http" => Ok(Self::Http),
         "messages" => Ok(Self::Messages),
         "hooks" => Ok(Self::Hooks),
         _ => Err(PermissionError::Unknown(name.to_owned())),
      }
   }
}

pub enum Permissions {
   /// The debug shell's prompt, which reaches everything.
   Console,
   Declared(BTreeSet<Permission>),
}

impl FromStr for Permissions {
   type Err = PermissionError;

   fn from_str(source: &str) -> Result<Self, Self::Err> {
      let mut declared = BTreeSet::new();
      for (key, value) in banner(source) {
         if key == "permissions" {
            for name in value.split_whitespace() {
               declared.insert(name.parse()?);
            }
         }
      }
      Ok(Self::Declared(declared))
   }
}

/// Hosts any plugin may point Discord at, since they reach only Discord.
fn discord_host(host: &str) -> bool {
   [
      "discord.com",
      "discordapp.com",
      "discord.gg",
      "discordapp.net",
   ]
   .iter()
   .any(|domain| {
      host == *domain
         || host
            .strip_suffix(domain)
            .is_some_and(|sub| sub.ends_with('.'))
   })
}

/// The mobile sink draws `…/badge-icons/phx_u_<hex>.png` from the URL the hex
/// spells, so a badge path counts as that URL.
const BADGE: &str = "phx_u_";

/// The `https://` URL `hex` spells.
#[must_use]
pub fn badge_target(hex: &str) -> Option<String> {
   if hex.is_empty() || !hex.len().is_multiple_of(2) {
      return None;
   }
   let bytes = hex
      .as_bytes()
      .as_chunks::<2>()
      .0
      .iter()
      .map(|&[high, low]| {
         let high = char::from(high).to_digit(16)?;
         let low = char::from(low).to_digit(16)?;
         u8::try_from((high << 4) | low).ok()
      })
      .collect::<Option<Vec<u8>>>()?;
   String::from_utf8(bytes)
      .ok()
      .filter(|url| url.starts_with("https://"))
}

/// A host a client could connect to for a URL.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum UrlHost {
   Named(String),
   /// Brackets, percent escapes or other bytes a host name lacks, which a
   /// client may decode into any host.
   Unreadable,
}

impl fmt::Display for UrlHost {
   fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
      match self {
         Self::Named(name) => formatter.write_str(name),
         Self::Unreadable => formatter.write_str("a host with escaped or bracketed bytes"),
      }
   }
}

/// The hosts after `http:` or `https:`. `OkHttp` ends the authority at `\` and
/// takes the last `@`, `java.net.URL` reads past `\` and takes the first,
/// WHATWG drops tabs and newlines, and the message linkifier stops at
/// whitespace, so each reading counts.
fn readings(rest: &str) -> BTreeSet<UrlHost> {
   let rest = rest.trim_start_matches(['/', '\\', '\t', '\n', '\r']);
   let wide = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
   let narrow = &wide[..wide.find('\\').unwrap_or(wide.len())];
   let word = &narrow[..narrow.find(char::is_whitespace).unwrap_or(narrow.len())];
   let joined = narrow.replace(['\t', '\n', '\r'], "");
   let mut found = BTreeSet::new();
   for authority in [wide, narrow, word, joined.as_str()] {
      let first = authority
         .split_once('@')
         .map_or(authority, |(_, host)| host);
      let last = authority
         .rsplit_once('@')
         .map_or(authority, |(_, host)| host);
      for userless in [first, last] {
         let host = userless.split(':').next().unwrap_or_default();
         if host.is_empty() || host.contains(char::is_whitespace) {
            continue;
         }
         let named = host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-');
         found.insert(if named {
            UrlHost::Named(host.to_ascii_lowercase())
         } else {
            UrlHost::Unreadable
         });
      }
   }
   found
}

/// The hosts `url` reaches when it is an `http(s)` URL.
#[must_use]
pub fn url_hosts(url: &str) -> Option<BTreeSet<UrlHost>> {
   let (scheme, rest) = url.split_once(':')?;
   if !(scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")) {
      return None;
   }
   Some(readings(rest)).filter(|found| !found.is_empty())
}

/// The first host name `url` reaches, for messages.
#[must_use]
pub fn url_host(url: &str) -> Option<String> {
   url_hosts(url)?.into_iter().find_map(|host| {
      match host {
         UrlHost::Named(name) => Some(name),
         UrlHost::Unreadable => None,
      }
   })
}

/// Whether `url` is `original` with query pairs dropped, as tracker stripping
/// leaves it. Which pairs go is still a choice an attacker's link offers.
fn trimmed(original: &str, url: &str) -> bool {
   fn split(text: &str) -> (&str, &str, Option<&str>) {
      let (rest, fragment) = text
         .split_once('#')
         .map_or((text, None), |(rest, fragment)| (rest, Some(fragment)));
      let (base, query) = rest.split_once('?').unwrap_or((rest, ""));
      (base, query, fragment)
   }

   let (base, query, fragment) = split(original);
   let (trimmed_base, trimmed_query, trimmed_fragment) = split(url);
   if base != trimmed_base || fragment != trimmed_fragment {
      return false;
   }
   let mut pairs = query.split('&');
   trimmed_query
      .split('&')
      .filter(|pair| !pair.is_empty())
      .all(|kept| pairs.any(|pair| pair == kept))
}

/// The `http(s)` URLs in a text, keyed by how the text spells them up to the
/// next whitespace, with the hosts each reaches. JSON is read string by
/// string, so escapes cannot hide a URL.
#[derive(Default)]
pub struct Links(BTreeMap<String, BTreeSet<UrlHost>>);

impl Links {
   #[must_use]
   pub fn scan(text: &str) -> Self {
      let mut links = Self::default();
      let mut json = serde_json::Deserializer::from_str(text);
      let walked = Strings(&mut links)
         .deserialize(&mut json)
         .and_then(|()| json.end());
      if walked.is_err() {
         links = Self::default();
         links.plain(text);
      }
      links
   }

   fn plain(&mut self, text: &str) {
      let bytes = text.as_bytes();
      for (colon, _) in text.match_indices(':') {
         let scheme = |name: &str| {
            colon
               .checked_sub(name.len())
               .filter(|&start| bytes[start..colon].eq_ignore_ascii_case(name.as_bytes()))
         };
         let Some(start) = scheme("https").or_else(|| scheme("http")) else {
            continue;
         };
         let found = readings(&text[colon + 1..]);
         if found.is_empty() {
            continue;
         }
         let end = text[start..]
            .find(char::is_whitespace)
            .map_or(text.len(), |length| start + length);
         let url = text[start..end].trim_end_matches([
            '.', ',', '!', '?', ';', ':', ']', ')', '>', '"', '\'', '*', '~',
         ]);
         self.0.entry(url.to_owned()).or_default().extend(found);
      }
      for (at, _) in text.match_indices(BADGE) {
         let hex = text[at + BADGE.len()..]
            .split(|byte: char| !byte.is_ascii_hexdigit())
            .next()
            .unwrap_or_default();
         if let Some(target) = badge_target(hex) {
            self.plain(&target);
         }
      }
   }

   pub fn urls(&self) -> impl Iterator<Item = &str> {
      self.0.keys().map(String::as_str)
   }
}

/// Feeds every string in a JSON document, keys included, to [`Links::plain`].
struct Strings<'links>(&'links mut Links);

impl<'de> DeserializeSeed<'de> for Strings<'_> {
   type Value = ();

   fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
      deserializer.deserialize_any(self)
   }
}

impl<'de> Visitor<'de> for Strings<'_> {
   type Value = ();

   fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
      formatter.write_str("JSON")
   }

   fn visit_str<E>(self, text: &str) -> Result<(), E> {
      self.0.plain(text);
      Ok(())
   }

   fn visit_bool<E>(self, _: bool) -> Result<(), E> {
      Ok(())
   }

   fn visit_i64<E>(self, _: i64) -> Result<(), E> {
      Ok(())
   }

   fn visit_u64<E>(self, _: u64) -> Result<(), E> {
      Ok(())
   }

   fn visit_f64<E>(self, _: f64) -> Result<(), E> {
      Ok(())
   }

   fn visit_unit<E>(self) -> Result<(), E> {
      Ok(())
   }

   fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
      while seq.next_element_seed(Strings(&mut *self.0))?.is_some() {}
      Ok(())
   }

   fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
      while map.next_key_seed(Strings(&mut *self.0))?.is_some() {
         map.next_value_seed(Strings(&mut *self.0))?;
      }
      Ok(())
   }
}

impl Permissions {
   /// What the plugin declared. The console declares nothing.
   #[must_use]
   pub fn declared(&self) -> Vec<Permission> {
      match self {
         Self::Console => Vec::new(),
         Self::Declared(declared) => declared.iter().copied().collect(),
      }
   }

   #[must_use]
   pub fn allows(&self, permission: Permission) -> bool {
      match self {
         Self::Console => true,
         Self::Declared(declared) => {
            declared.contains(&permission) || declared.contains(&Permission::Hooks)
         },
      }
   }

   /// Whether the plugin may fetch `url`, an `http(s)` URL.
   #[must_use]
   pub fn fetch(&self, url: &str) -> bool {
      self.allows(Permission::Http) && url_hosts(url).is_some()
   }

   /// Whether the plugin may point Discord at `host`.
   #[must_use]
   pub fn host(&self, host: &str) -> bool {
      discord_host(host) || self.allows(Permission::Http)
   }

   /// The first host in `found` the plugin may not point Discord at.
   fn refused<'found>(&self, found: &'found BTreeSet<UrlHost>) -> Option<&'found UrlHost> {
      found.iter().find(|host| {
         match host {
            UrlHost::Named(name) => !self.host(name),
            UrlHost::Unreadable => !self.allows(Permission::Http),
         }
      })
   }
   /// Whether the plugin may draw an image from `uri`. Names like
   /// `res://ic_mana_id` and inline SVG paths load nothing.
   #[must_use]
   pub fn image(&self, uri: &str) -> bool {
      let lower = uri.trim_start().to_ascii_lowercase();
      if !["http:", "https:", "//"]
         .iter()
         .any(|scheme| lower.starts_with(scheme))
      {
         return true;
      }
      let mut links = Links::default();
      links.plain(uri);
      !links.0.is_empty() && links.0.values().all(|found| self.refused(found).is_none())
   }

   /// Whether the plugin may open `url` in the browser: Discord's own links,
   /// or any web link with `http`.
   #[must_use]
   pub fn link(&self, url: &str) -> bool {
      url.starts_with("discord://")
         || url_hosts(url).is_some_and(|found| self.refused(&found).is_none())
   }

   /// The first host a URL in `after` reaches that the plugin may not point
   /// Discord at, unless `before` spelled the same URL or one it trims down to,
   /// or `seen` vouches for it.
   #[must_use]
   pub fn added(
      &self,
      before: &Links,
      after: &Links,
      seen: impl Fn(&str) -> bool,
   ) -> Option<UrlHost> {
      self.foreign(after, |url| {
         before.0.contains_key(url)
            || before.urls().any(|original| trimmed(original, url))
            || seen(url)
      })
   }

   /// The first host a URL in `links` reaches that the plugin may not point
   /// Discord at, among the URLs `known` does not vouch for.
   #[must_use]
   pub fn foreign(&self, links: &Links, known: impl Fn(&str) -> bool) -> Option<UrlHost> {
      links
         .0
         .iter()
         .filter(|(url, _)| !known(url))
         .find_map(|(_, found)| self.refused(found))
         .cloned()
   }

   /// What `newer` declares that this does not, so an update cannot widen
   /// what a plugin reaches unasked.
   #[must_use]
   pub fn widened(&self, newer: &Self) -> Vec<Permission> {
      newer
         .declared()
         .into_iter()
         .filter(|&permission| !self.allows(permission))
         .collect()
   }
}

#[cfg(test)]
mod tests {
   use super::*;

   #[test]
   fn links_off_discord_need_http() {
      let none = Permissions::Declared(BTreeSet::new());
      assert!(none.link("https://discord.com/channels/1/2"));
      assert!(none.link("discord://-/channels/1/2"));
      assert!(!none.link("https://lens.google.com/uploadbyurl?url=x"));
      assert!(!none.link("https://discord.com@example.com/"));
      assert!(!none.link("file:///C:/Windows/notepad.exe"));
      let http = Permissions::Declared(BTreeSet::from([Permission::Http]));
      assert!(http.link("https://lens.google.com/uploadbyurl?url=x"));
   }
}
