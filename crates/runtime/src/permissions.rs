//! What a plugin's banner lets it reach. A plugin's `phoenix` object leaves out
//! what it did not declare, and the calls that take a route, host or event
//! refuse the ones outside its lines.
//!
//! - `// @gateway messageCreate messageUpdate` for gateway hook points.
//! - `// @gateway-send 4` for outgoing gateway opcodes.
//! - `// @discord GET /channels/*/messages` for `phoenix.discord` requests and
//!   interceptors, one route a line. `*` is one path segment, a final `**` the
//!   rest, and `*` in place of the method any method.
//! - `// @fetch translate.googleapis.com *.example.com` for `phoenix.fetch`.
//! - `// @stores`, `// @messages` and `// @bridge` for those namespaces whole.
//! - `// @ui-tree` for the raw view tree, which holds whatever the screen
//!   shows.
//!
//! Images Discord draws reach the network too. An icon a plugin draws, and a
//! URL host a hook adds to what Discord renders, must be one of its `@fetch`
//! hosts or Discord's own.

use std::{
   collections::BTreeSet,
   fmt,
   str::FromStr,
};

use crate::banner;

#[derive(Debug, flub::Error)]
pub enum PermissionError {
   #[error("@gateway-send {0}: opcodes are integers")]
   Opcode(String),
   #[error("@discord {0}: write a method and a path starting with /, like GET /users/@me")]
   Route(String),
   #[error("@discord {0}: ** only ends a route")]
   Rest(String),
   #[error("@fetch: {0} is not a host name")]
   Host(String),
}

pub enum Permissions {
   /// The debug shell's prompt, which reaches everything.
   Console,
   Declared(Declared),
}

#[derive(Default)]
pub struct Declared {
   gateway:      BTreeSet<String>,
   gateway_send: BTreeSet<u32>,
   discord:      Vec<Route>,
   fetch:        Vec<Host>,
   whole:        BTreeSet<Namespace>,
}

/// The namespaces a plugin reaches whole or not at all.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Namespace {
   Stores,
   Messages,
   Bridge,
   UiTree,
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

/// Every URL host in `text`, JSON with escaped slashes included, and the hosts
/// badge paths spell.
#[must_use]
pub fn hosts(text: &str) -> BTreeSet<String> {
   let text = text.replace("\\/", "/");
   let lower = text.to_ascii_lowercase();
   let mut found = BTreeSet::new();
   for (at, _) in lower.match_indices("://") {
      if !(lower[..at].ends_with("http") || lower[..at].ends_with("https")) {
         continue;
      }
      let host = lower[at + 3..]
         .split(|byte: char| !(byte.is_ascii_alphanumeric() || byte == '.' || byte == '-'))
         .next()
         .unwrap_or_default();
      if !host.is_empty() {
         found.insert(host.to_owned());
      }
   }
   for (at, _) in text.match_indices(BADGE) {
      let hex = text[at + BADGE.len()..]
         .split(|byte: char| !byte.is_ascii_hexdigit())
         .next()
         .unwrap_or_default();
      if let Some(target) = badge_target(hex) {
         found.extend(hosts(&target));
      }
   }
   found
}

struct Route {
   method:   Option<String>,
   segments: Vec<Segment>,
}

/// One thing a plugin reaches, as its settings page lists it.
#[derive(Clone, Debug)]
pub enum Grant {
   Events(Vec<String>),
   GatewaySend(Vec<u32>),
   Discord(String),
   Fetch(Vec<String>),
   Stores,
   Messages,
   Bridge,
   UiTree,
}

impl Grant {
   #[must_use]
   pub const fn label(&self) -> &'static str {
      match self {
         Self::Events(_) => "Gateway events",
         Self::GatewaySend(_) => "Gateway sends",
         Self::Discord(_) => "Discord API",
         Self::Fetch(_) => "Web hosts",
         Self::Stores => "Message cache",
         Self::Messages => "Outgoing messages",
         Self::Bridge => "Host modules",
         Self::UiTree => "Screen contents",
      }
   }
}

impl fmt::Display for Grant {
   fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
      match self {
         Self::Events(names) | Self::Fetch(names) => formatter.write_str(&names.join(", ")),
         Self::GatewaySend(ops) => {
            let ops = ops.iter().map(u32::to_string).collect::<Vec<_>>();
            write!(formatter, "opcode {}", ops.join(", "))
         },
         Self::Discord(route) => formatter.write_str(route),
         Self::Stores => formatter.write_str("read"),
         Self::Messages => formatter.write_str("read and rewrite"),
         Self::Bridge => formatter.write_str("full access"),
         Self::UiTree => formatter.write_str("read and change"),
      }
   }
}

impl fmt::Display for Route {
   fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
      formatter.write_str(self.method.as_deref().unwrap_or("*"))?;
      formatter.write_str(" ")?;
      for segment in &self.segments {
         formatter.write_str("/")?;
         match segment {
            Segment::Literal(literal) => formatter.write_str(literal)?,
            Segment::Any => formatter.write_str("*")?,
            Segment::Rest => formatter.write_str("**")?,
         }
      }
      Ok(())
   }
}

impl fmt::Display for Host {
   fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
      match self {
         Self::Exact(name) => formatter.write_str(name),
         Self::Under(name) => write!(formatter, "*.{name}"),
      }
   }
}

enum Segment {
   Literal(String),
   Any,
   Rest,
}

enum Host {
   Exact(String),
   /// `*.example.com`, which matches `cdn.example.com` and not `example.com`.
   Under(String),
}

impl FromStr for Permissions {
   type Err = PermissionError;

   fn from_str(source: &str) -> Result<Self, Self::Err> {
      let mut declared = Declared::default();
      for (key, value) in banner(source) {
         match key {
            "gateway" => {
               declared
                  .gateway
                  .extend(value.split_whitespace().map(str::to_owned));
            },
            "gateway-send" => {
               for op in value.split_whitespace() {
                  let op = op
                     .parse()
                     .map_err(|_| PermissionError::Opcode(op.to_owned()))?;
                  declared.gateway_send.insert(op);
               }
            },
            "discord" => declared.discord.push(value.parse()?),
            "fetch" => {
               for host in value.split_whitespace() {
                  declared.fetch.push(host.parse()?);
               }
            },
            "stores" => _ = declared.whole.insert(Namespace::Stores),
            "messages" => _ = declared.whole.insert(Namespace::Messages),
            "bridge" => _ = declared.whole.insert(Namespace::Bridge),
            "ui-tree" => _ = declared.whole.insert(Namespace::UiTree),
            _ => {},
         }
      }
      Ok(Self::Declared(declared))
   }
}

impl FromStr for Route {
   type Err = PermissionError;

   fn from_str(value: &str) -> Result<Self, Self::Err> {
      let invalid = || PermissionError::Route(value.to_owned());
      let (method, path) = value.split_once(' ').ok_or_else(invalid)?;
      let path = path.trim().strip_prefix('/').ok_or_else(invalid)?;
      let parts = path.split('/').collect::<Vec<_>>();
      let segments = parts
         .iter()
         .enumerate()
         .map(|(index, part)| {
            match *part {
               "**" if index + 1 == parts.len() => Ok(Segment::Rest),
               "**" => Err(PermissionError::Rest(value.to_owned())),
               "*" => Ok(Segment::Any),
               literal => Ok(Segment::Literal(literal.to_owned())),
            }
         })
         .collect::<Result<_, _>>()?;
      Ok(Self {
         method: (method != "*").then(|| method.to_ascii_uppercase()),
         segments,
      })
   }
}

impl Route {
   fn matches(&self, method: &str, path: &str) -> bool {
      if self
         .method
         .as_ref()
         .is_some_and(|expected| !method.eq_ignore_ascii_case(expected))
      {
         return false;
      }
      let path = path.split(['?', '#']).next().unwrap_or(path);
      let Some(path) = path.strip_prefix('/') else {
         return false;
      };
      let dots = |part: &str| {
         let decoded = part.to_ascii_lowercase().replace("%2e", ".");
         decoded == "." || decoded == ".."
      };
      if path.split('/').any(dots) {
         return false;
      }
      let mut parts = path.split('/');
      for segment in &self.segments {
         match (segment, parts.next()) {
            (Segment::Rest, _) => return true,
            (Segment::Any, Some(part)) if !part.is_empty() => {},
            (Segment::Literal(literal), Some(part)) if literal == part => {},
            _ => return false,
         }
      }
      parts.next().is_none()
   }
}

impl FromStr for Host {
   type Err = PermissionError;

   fn from_str(value: &str) -> Result<Self, Self::Err> {
      let host = value.to_ascii_lowercase();
      let (under, name) = host
         .strip_prefix("*.")
         .map_or((false, host.as_str()), |name| (true, name));
      let valid = name.contains('.')
         && name.split('.').all(|label| {
            !label.is_empty()
               && label
                  .bytes()
                  .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
         });
      if !valid {
         return Err(PermissionError::Host(value.to_owned()));
      }
      let name = name.to_owned();
      Ok(if under {
         Self::Under(name)
      } else {
         Self::Exact(name)
      })
   }
}

impl Host {
   fn matches(&self, host: &str) -> bool {
      match self {
         Self::Exact(name) => host == name,
         Self::Under(name) => {
            host
               .strip_suffix(name.as_str())
               .is_some_and(|sub| sub.len() > 1 && sub.ends_with('.'))
         },
      }
   }
}

/// The host of an `http(s)://` URL, lowercased, after any `user@` part.
#[must_use]
pub fn url_host(url: &str) -> Option<String> {
   let rest = url
      .strip_prefix("https://")
      .or_else(|| url.strip_prefix("http://"))?;
   let authority = rest.split(['/', '?', '#']).next()?;
   let authority = authority.rsplit('@').next()?;
   let host = authority.split(':').next()?;
   (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

impl Permissions {
   /// What the plugin declared. The console declares nothing.
   #[must_use]
   pub fn grants(&self) -> Vec<Grant> {
      let Self::Declared(declared) = self else {
         return Vec::new();
      };
      let mut grants = Vec::new();
      if !declared.gateway.is_empty() {
         grants.push(Grant::Events(declared.gateway.iter().cloned().collect()));
      }
      if !declared.gateway_send.is_empty() {
         grants.push(Grant::GatewaySend(
            declared.gateway_send.iter().copied().collect(),
         ));
      }
      grants.extend(
         declared
            .discord
            .iter()
            .map(|route| Grant::Discord(route.to_string())),
      );
      if !declared.fetch.is_empty() {
         grants.push(Grant::Fetch(
            declared.fetch.iter().map(Host::to_string).collect(),
         ));
      }
      grants.extend(declared.whole.iter().map(|namespace| {
         match namespace {
            Namespace::Stores => Grant::Stores,
            Namespace::Messages => Grant::Messages,
            Namespace::Bridge => Grant::Bridge,
            Namespace::UiTree => Grant::UiTree,
         }
      }));
      grants
   }

   #[must_use]
   pub fn event(&self, name: &str) -> bool {
      match self {
         Self::Console => true,
         Self::Declared(declared) => declared.gateway.contains(name),
      }
   }

   #[must_use]
   pub fn gateway_send(&self, op: u32) -> bool {
      match self {
         Self::Console => true,
         Self::Declared(declared) => declared.gateway_send.contains(&op),
      }
   }

   #[must_use]
   pub fn discord(&self, method: &str, path: &str) -> bool {
      match self {
         Self::Console => true,
         Self::Declared(declared) => {
            declared
               .discord
               .iter()
               .any(|route| route.matches(method, path))
         },
      }
   }

   #[must_use]
   pub fn fetch(&self, url: &str) -> bool {
      match self {
         Self::Console => true,
         Self::Declared(declared) => {
            url_host(url)
               .is_some_and(|host| declared.fetch.iter().any(|allowed| allowed.matches(&host)))
         },
      }
   }

   #[must_use]
   pub fn stores(&self) -> bool {
      self.whole(Namespace::Stores)
   }

   #[must_use]
   pub fn messages(&self) -> bool {
      self.whole(Namespace::Messages)
   }

   #[must_use]
   pub fn bridge(&self) -> bool {
      self.whole(Namespace::Bridge)
   }

   #[must_use]
   pub fn ui_tree(&self) -> bool {
      self.whole(Namespace::UiTree)
   }

   fn whole(&self, namespace: Namespace) -> bool {
      match self {
         Self::Console => true,
         Self::Declared(declared) => declared.whole.contains(&namespace),
      }
   }

   /// Whether the plugin may point Discord at `host`.
   #[must_use]
   pub fn host(&self, host: &str) -> bool {
      match self {
         Self::Console => true,
         Self::Declared(declared) => {
            discord_host(host) || declared.fetch.iter().any(|allowed| allowed.matches(host))
         },
      }
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
      let found = hosts(uri);
      !found.is_empty() && found.iter().all(|host| self.host(host))
   }

   /// The first URL host `after` has that `before` lacks and the plugin may
   /// not point Discord at.
   #[must_use]
   pub fn added(&self, before: &str, after: &str) -> Option<String> {
      let known = hosts(before);
      hosts(after)
         .into_iter()
         .find(|host| !known.contains(host) && !self.host(host))
   }
}
