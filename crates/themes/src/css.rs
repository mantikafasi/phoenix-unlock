//! `.theme.css` metadata, selector-scoped custom properties, and recursive
//! `var(--name, fallback)` resolution.

use std::collections::{
   BTreeMap,
   HashSet,
};

use serde::{
   Deserialize,
   Serialize,
};

/// Whether a theme or block targets Discord's dark or light mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
   Dark,
   Light,
}

/// Header metadata extracted from leading `/* ... */` comments in `.theme.css`
/// or from a JSON manifest.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeMeta {
   #[serde(default)]
   pub name:        String,
   #[serde(default, skip_serializing_if = "Option::is_none")]
   pub description: Option<String>,
   #[serde(default, skip_serializing_if = "Option::is_none")]
   pub author:      Option<String>,
   #[serde(default, skip_serializing_if = "Option::is_none")]
   pub version:     Option<String>,
   #[serde(default, skip_serializing_if = "Option::is_none")]
   pub source:      Option<String>,
   #[serde(default, skip_serializing_if = "Option::is_none")]
   pub mode:        Option<ThemeMode>,
}

/// Raw custom properties extracted from a `.theme.css` stylesheet, split by
/// `:root` vs `.theme-dark` vs `.theme-light` selector blocks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedCss {
   pub meta:  ThemeMeta,
   pub root:  BTreeMap<String, String>,
   pub dark:  BTreeMap<String, String>,
   pub light: BTreeMap<String, String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
   Both,
   Dark,
   Light,
}

/// Returns whether a single simple selector token is a global/root or Discord
/// theme scope token (rejecting component selectors like `.sidebar`).
fn is_global_token(token: &str) -> bool {
   matches!(
      token,
      ":root"
         | "html"
         | "body"
         | "*"
         | "#app-mount"
         | ".theme-dark"
         | ".theme-light"
         | ".theme-darker"
         | ".theme-midnight"
         | ".visual-refresh"
         | ".app-focused"
   )
}

/// Splits a compound selector (like `:root.theme-dark` or `html.theme-light`)
/// into its constituent simple tokens and classifies its scope, or returns
/// `None` if it targets a component (such as `.sidebar` or `.chat`).
fn classify_single_selector(selector: &str) -> Option<Scope> {
   let trimmed = selector.trim();
   if trimmed.is_empty() {
      return None;
   }
   // Reject combinators (`>`, `+`, `~`, or descendant whitespace) unless every
   // segment is itself a global root/theme wrapper (e.g. `html body.theme-dark`).
   let mut has_dark = false;
   let mut has_light = false;
   let mut any_token = false;
   for part in trimmed.split_ascii_whitespace() {
      if matches!(part, ">" | "+" | "~") {
         return None;
      }
      // Split compound tokens at `.`, `#`, `:` boundaries.
      let lower = part.to_ascii_lowercase();
      let mut start = 0usize;
      let bytes = lower.as_bytes();
      for i in 1..bytes.len() {
         if matches!(bytes[i], b'.' | b'#' | b':') {
            let tok = &lower[start..i];
            if !is_global_token(tok) {
               return None;
            }
            has_dark |= matches!(tok, ".theme-dark" | ".theme-darker" | ".theme-midnight");
            has_light |= tok == ".theme-light";
            start = i;
         }
      }
      let last = &lower[start..];
      if !is_global_token(last) {
         return None;
      }
      has_dark |= matches!(last, ".theme-dark" | ".theme-darker" | ".theme-midnight");
      has_light |= last == ".theme-light";
      any_token = true;
   }
   if !any_token {
      return None;
   }
   match (has_dark, has_light) {
      (true, false) => Some(Scope::Dark),
      (false, true) => Some(Scope::Light),
      _ => Some(Scope::Both),
   }
}

/// Classifies a comma-separated selector list by unioning the scopes of all
/// global/theme selectors in the list (skipping component-only rules).
fn classify_selector(selector_list: &str) -> Option<Scope> {
   let mut want_dark = false;
   let mut want_light = false;
   for part in selector_list.split(',') {
      match classify_single_selector(part) {
         Some(Scope::Both) => {
            want_dark = true;
            want_light = true;
         },
         Some(Scope::Dark) => want_dark = true,
         Some(Scope::Light) => want_light = true,
         None => {},
      }
   }
   match (want_dark, want_light) {
      (true, true) => Some(Scope::Both),
      (true, false) => Some(Scope::Dark),
      (false, true) => Some(Scope::Light),
      (false, false) => None,
   }
}
/// Strips `/* ... */` comments while reading `@name`, `@description`,
/// `@author`, `@version`, `@source`, and `@mode` tags.
fn strip_comments_and_meta(css: &str) -> (String, ThemeMeta) {
   let mut clean = String::with_capacity(css.len());
   let mut meta = ThemeMeta::default();
   let mut rest = css;
   while let Some(start) = rest.find("/*") {
      clean.push_str(&rest[..start]);
      let Some(len) = rest[start + 2..].find("*/") else {
         return (clean, meta);
      };
      let comment = &rest[start + 2..start + 2 + len];
      for raw_line in comment.lines() {
         let line = raw_line.trim().trim_start_matches('*').trim();
         let Some(tagged) = line.strip_prefix('@') else {
            continue;
         };
         let (tag, value) = tagged
            .split_once(|c: char| c.is_ascii_whitespace())
            .unwrap_or((tagged, ""));
         let value = value.trim();
         if value.is_empty() {
            continue;
         }
         match tag.to_ascii_lowercase().as_str() {
            "name" if meta.name.is_empty() => value.clone_into(&mut meta.name),
            "description" if meta.description.is_none() => {
               meta.description = Some(value.to_owned());
            },
            "author" if meta.author.is_none() => meta.author = Some(value.to_owned()),
            "version" if meta.version.is_none() => meta.version = Some(value.to_owned()),
            "source" | "website" if meta.source.is_none() => {
               meta.source = Some(value.to_owned());
            },
            "mode" if meta.mode.is_none() => {
               meta.mode = match value.to_ascii_lowercase().as_str() {
                  "dark" => Some(ThemeMode::Dark),
                  "light" => Some(ThemeMode::Light),
                  _ => None,
               };
            },
            _ => {},
         }
      }
      rest = &rest[start + 2 + len + 2..];
   }
   clean.push_str(rest);
   (clean, meta)
}

/// Extracts `--name: value` declarations from a single declaration block body.
fn parse_declarations(body: &str, out: &mut BTreeMap<String, String>) {
   let mut pos = 0;
   let bytes = body.as_bytes();
   while pos < bytes.len() {
      // Skip whitespace and semicolons.
      while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\r' | b'\n' | b';') {
         pos += 1;
      }
      if pos >= bytes.len() {
         break;
      }
      // Scan to the end of this declaration, honoring quotes and parentheses so
      // `url("data:image/png;base64,...")` does not split on `;`.
      let start = pos;
      let mut paren = 0usize;
      let mut quote: Option<u8> = None;
      let mut colon = None;
      while pos < bytes.len() {
         let b = bytes[pos];
         if let Some(q) = quote {
            if b == b'\\' && pos + 1 < bytes.len() {
               pos += 2;
               continue;
            }
            if b == q {
               quote = None;
            }
         } else {
            match b {
               b'"' | b'\'' => quote = Some(b),
               b'(' => paren += 1,
               b')' => paren = paren.saturating_sub(1),
               b':' if paren == 0 && colon.is_none() => colon = Some(pos),
               b';' if paren == 0 => break,
               _ => {},
            }
         }
         pos += 1;
      }
      let end = pos;
      if pos < bytes.len() && bytes[pos] == b';' {
         pos += 1;
      }
      let Some(colon) = colon else {
         continue;
      };
      let name = body[start..colon].trim();
      if !name.starts_with("--") || name.contains([' ', '\t', '\r', '\n', '{', '}', '(', ')']) {
         continue;
      }
      let raw_val = body[colon + 1..end].trim();
      let val = strip_important(raw_val);
      if !val.is_empty() {
         out.insert(name.to_ascii_lowercase(), val.to_owned());
      }
   }
}

/// Strips a trailing `!important` token only when it appears outside quotes at
/// the end of `value`.
fn strip_important(value: &str) -> &str {
   let trimmed = value.trim();
   let bytes = trimmed.as_bytes();
   let mut quote: Option<u8> = None;
   let mut last_bang = None;
   let mut i = 0;
   while i < bytes.len() {
      let b = bytes[i];
      if let Some(q) = quote {
         if b == b'\\' && i + 1 < bytes.len() {
            i += 2;
            continue;
         }
         if b == q {
            quote = None;
         }
      } else if b == b'"' || b == b'\'' {
         quote = Some(b);
      } else if b == b'!' {
         last_bang = Some(i);
      }
      i += 1;
   }
   if let Some(bang) = last_bang {
      let tail = trimmed[bang + 1..].trim();
      if tail.eq_ignore_ascii_case("important") {
         return trimmed[..bang].trim();
      }
   }
   trimmed
}

fn classify_media(selector: &str) -> Option<Scope> {
   let lower = selector.to_ascii_lowercase();
   let rest = lower.strip_prefix("@media")?;
   if !rest.contains("prefers-color-scheme") {
      return None;
   }
   let wants_dark = rest.contains("dark");
   let wants_light = rest.contains("light");
   match (wants_dark, wants_light) {
      (true, false) => Some(Scope::Dark),
      (false, true) => Some(Scope::Light),
      _ => None,
   }
}

const MAX_MEDIA_DEPTH: usize = 8;

/// Parses `.theme.css` into header metadata and scoped custom property maps.
#[must_use]
pub fn parse_css(css: &str) -> ParsedCss {
   let (clean, meta) = strip_comments_and_meta(css);
   let mut parsed = ParsedCss {
      meta,
      ..ParsedCss::default()
   };
   let found_block = parse_blocks(&clean, 0, None, &mut parsed);

   // Support bare `--var: val;` quick-css snippets without a selector block.
   if !found_block {
      parse_declarations(&clean, &mut parsed.root);
   }

   parsed
}

fn parse_blocks(
   clean: &str,
   depth: usize,
   forced_scope: Option<Scope>,
   parsed: &mut ParsedCss,
) -> bool {
   if depth > MAX_MEDIA_DEPTH {
      return false;
   }
   let bytes = clean.as_bytes();
   let mut pos = 0;
   let mut found_block = false;
   while pos < bytes.len() {
      let mut sel_start = pos;
      let mut paren = 0usize;
      let mut quote: Option<u8> = None;
      while pos < bytes.len() {
         let b = bytes[pos];
         if let Some(q) = quote {
            if b == b'\\' && pos + 1 < bytes.len() {
               pos += 2;
               continue;
            }
            if b == q {
               quote = None;
            }
         } else {
            match b {
               b'"' | b'\'' => quote = Some(b),
               b'(' => paren += 1,
               b')' => paren = paren.saturating_sub(1),
               // Skip top-level semicolon-terminated statements (`@import`,
               // `@charset`, etc.) so they never swallow the next selector.
               b';' if paren == 0 => sel_start = pos + 1,
               b'{' => break,
               _ => {},
            }
         }
         pos += 1;
      }
      if pos >= bytes.len() {
         break;
      }
      let selector = clean[sel_start..pos].trim();
      let body_start = pos + 1;
      pos += 1;
      let mut brace_depth = 1usize;
      quote = None;
      while pos < bytes.len() && brace_depth > 0 {
         let b = bytes[pos];
         if let Some(q) = quote {
            if b == b'\\' && pos + 1 < bytes.len() {
               pos += 2;
               continue;
            }
            if b == q {
               quote = None;
            }
         } else {
            match b {
               b'"' | b'\'' => quote = Some(b),
               b'{' => brace_depth += 1,
               b'}' => brace_depth -= 1,
               _ => {},
            }
         }
         pos += 1;
      }
      let body_end = if brace_depth == 0 { pos - 1 } else { pos };
      let body = &clean[body_start..body_end];
      found_block = true;

      // Honor `@media (prefers-color-scheme: dark|light)` up to `MAX_MEDIA_DEPTH`
      // without re-stripping comments, and skip other at-rules.
      if selector.starts_with('@') {
         if depth < MAX_MEDIA_DEPTH
            && body.contains('{')
            && let Some(media_scope) = classify_media(selector)
         {
            let combined = match (forced_scope, media_scope) {
               (None, s) => Some(s),
               (Some(prev), s) if prev == s => Some(s),
               _ => None,
            };
            if let Some(next_scope) = combined {
               parse_blocks(body, depth + 1, Some(next_scope), parsed);
            }
         }
         continue;
      }

      let Some(sel_scope) = classify_selector(selector) else {
         continue;
      };
      let effective = match (forced_scope, sel_scope) {
         (None, s) => Some(s),
         (Some(f), Scope::Both) => Some(f),
         (Some(f), s) if f == s => Some(f),
         _ => None,
      };
      let Some(scope) = effective else {
         continue;
      };
      let target = match scope {
         Scope::Both => &mut parsed.root,
         Scope::Dark => &mut parsed.dark,
         Scope::Light => &mut parsed.light,
      };
      parse_declarations(body, target);
   }
   found_block
}

const MAX_VAR_DEPTH: usize = 16;
const MAX_VAR_EXPANSIONS: usize = 4096;
const MAX_VAR_BYTES: usize = 16_384;

/// Finds the first ASCII-case-insensitive `var(` call in `s`.
fn find_var_call(s: &str) -> Option<usize> {
   let bytes = s.as_bytes();
   bytes
      .windows(4)
      .position(|w| w.eq_ignore_ascii_case(b"var("))
}

struct VarResolver<'a> {
   vars:       &'a BTreeMap<String, String>,
   memo:       BTreeMap<String, String>,
   visiting:   HashSet<String>,
   expansions: usize,
}

impl<'a> VarResolver<'a> {
   fn new(vars: &'a BTreeMap<String, String>) -> Self {
      Self {
         vars,
         memo: BTreeMap::new(),
         visiting: HashSet::new(),
         expansions: 0,
      }
   }

   fn resolve_key(&mut self, key: &str, depth: usize) -> String {
      if let Some(cached) = self.memo.get(key) {
         return cached.clone();
      }
      let Some(raw) = self.vars.get(key) else {
         return String::new();
      };
      if depth >= MAX_VAR_DEPTH || self.expansions >= MAX_VAR_EXPANSIONS {
         return raw.clone();
      }
      self.visiting.insert(key.to_owned());
      let resolved = self.expand_str(raw, depth + 1);
      self.visiting.remove(key);
      self.memo.insert(key.to_owned(), resolved.clone());
      resolved
   }

   fn expand_str(&mut self, input: &str, depth: usize) -> String {
      if depth >= MAX_VAR_DEPTH
         || self.expansions >= MAX_VAR_EXPANSIONS
         || find_var_call(input).is_none()
      {
         return input.to_owned();
      }
      let mut out = String::with_capacity(input.len().min(MAX_VAR_BYTES));
      let mut rest = input;
      while let Some(idx) = find_var_call(rest) {
         push_capped(&mut out, &rest[..idx]);
         if out.len() >= MAX_VAR_BYTES || self.expansions >= MAX_VAR_EXPANSIONS {
            return out;
         }
         self.expansions += 1;
         let after = &rest[idx + 4..];
         let Some((inner, tail)) = split_balanced_paren(after) else {
            push_capped(&mut out, &rest[idx..]);
            return out;
         };
         let (raw_name, fallback) = split_top_level_comma(inner);
         let key = raw_name.trim().to_ascii_lowercase();
         if key.starts_with("--") && self.vars.contains_key(&key) && !self.visiting.contains(&key) {
            let resolved = self.resolve_key(&key, depth);
            push_capped(&mut out, &resolved);
         } else if let Some(fb) = fallback {
            let resolved = self.expand_str(fb.trim(), depth + 1);
            push_capped(&mut out, &resolved);
         }
         rest = tail;
      }
      push_capped(&mut out, rest);
      out
   }
}

fn push_capped(out: &mut String, piece: &str) {
   let remain = MAX_VAR_BYTES.saturating_sub(out.len());
   if remain == 0 {
      return;
   }
   if piece.len() <= remain {
      out.push_str(piece);
   } else {
      let mut end = remain;
      while end > 0 && !piece.is_char_boundary(end) {
         end -= 1;
      }
      out.push_str(&piece[..end]);
   }
}

/// Resolves all `var(--name)` and `var(--name, fallback)` references inside
/// `value` against `vars`, guarding against cycles, fan-out, and excessive
/// nesting.
#[must_use]
pub fn resolve_vars(value: &str, vars: &BTreeMap<String, String>) -> String {
   let mut resolver = VarResolver::new(vars);
   resolver.expand_str(value, 0)
}

/// Resolves every custom property in `vars` once using memoized DFS with
/// cycle detection and expansion/size budgets.
#[must_use]
pub fn resolve_all_vars(vars: &BTreeMap<String, String>) -> BTreeMap<String, String> {
   let mut resolver = VarResolver::new(vars);
   let mut out = BTreeMap::new();
   for key in vars.keys() {
      let value = resolver.resolve_key(key, 0);
      out.insert(key.clone(), value);
   }
   out
}

fn split_balanced_paren(s: &str) -> Option<(&str, &str)> {
   let bytes = s.as_bytes();
   let mut depth = 1usize;
   let mut quote: Option<u8> = None;
   let mut i = 0;
   while i < bytes.len() {
      let b = bytes[i];
      if let Some(q) = quote {
         if b == b'\\' && i + 1 < bytes.len() {
            i += 2;
            continue;
         }
         if b == q {
            quote = None;
         }
      } else {
         match b {
            b'"' | b'\'' => quote = Some(b),
            b'(' => depth += 1,
            b')' => {
               depth -= 1;
               if depth == 0 {
                  return Some((&s[..i], &s[i + 1..]));
               }
            },
            _ => {},
         }
      }
      i += 1;
   }
   None
}

fn split_top_level_comma(s: &str) -> (&str, Option<&str>) {
   let bytes = s.as_bytes();
   let mut depth = 0usize;
   let mut quote: Option<u8> = None;
   let mut i = 0;
   while i < bytes.len() {
      let b = bytes[i];
      if let Some(q) = quote {
         if b == b'\\' && i + 1 < bytes.len() {
            i += 2;
            continue;
         }
         if b == q {
            quote = None;
         }
      } else {
         match b {
            b'"' | b'\'' => quote = Some(b),
            b'(' => depth += 1,
            b')' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => return (&s[..i], Some(&s[i + 1..])),
            _ => {},
         }
      }
      i += 1;
   }
   (s, None)
}
