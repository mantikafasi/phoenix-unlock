//! A plugin's error placed in the plugin's own source, for the code editor and
//! the plugin page.

use std::fmt;

use rquickjs::CaughtError;

/// Lines `Function` puts before the body [`super::compile`] hands it:
/// `(function anonymous(phoenix` and `) {` (quickjs-ng's
/// `js_function_constructor`), so the engine's line 3 is the source's line 1.
const WRAPPER_LINES: usize = 2;

/// The file name `QuickJS` gives code it compiles for `Function`.
const INPUT: &str = "<input>:";

/// A plugin's error, with where it is in the plugin's source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceError {
   /// The error's name and message, like `SyntaxError: unexpected end of
   /// string`.
   pub message: String,
   pub at:      At,
}

/// Where a [`SourceError`] points in the source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum At {
   /// A line and column, both counted from 1.
   Line { line: usize, column: usize },
   /// After the last line, where a missing `}` or `)` shows.
   End,
   /// Nowhere in the source, like a header asking for a newer Sunbird.
   Nowhere,
}

impl SourceError {
   #[must_use]
   pub const fn nowhere(message: String) -> Self {
      Self {
         message,
         at: At::Nowhere,
      }
   }

   /// What `err`, caught while compiling or starting `source`, says, placed
   /// at the innermost frame of the plugin's code.
   #[must_use]
   pub fn caught(err: &CaughtError<'_>, source: &str) -> Self {
      let CaughtError::Exception(exception) = err else {
         return Self::nowhere(err.to_string().trim_end().to_owned());
      };
      let name = exception
         .as_object()
         .get::<_, Option<String>>("name")
         .ok()
         .flatten()
         .filter(|name| !name.is_empty());
      let message = match (name, exception.message().filter(|text| !text.is_empty())) {
         (Some(name), Some(text)) => format!("{name}: {text}"),
         (Some(only), None) | (None, Some(only)) => only,
         (None, None) => "the plugin threw an error".to_owned(),
      };
      let at = exception
         .stack()
         .and_then(|stack| first_input_frame(&stack))
         .map_or(At::Nowhere, |(line, column)| at(source, line, column));
      Self { message, at }
   }
}

impl fmt::Display for SourceError {
   fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
      match self.at {
         At::Line { line, column } => {
            write!(f, "{} (line {line}, column {column})", self.message)
         },
         At::End => write!(f, "{} (at the end of the code)", self.message),
         At::Nowhere => f.write_str(&self.message),
      }
   }
}

/// The engine's line and column in the first `<input>:line:column` of
/// `stack`, the innermost frame of plugin code.
fn first_input_frame(stack: &str) -> Option<(usize, usize)> {
   let rest = &stack[stack.find(INPUT)? + INPUT.len()..];
   let (line, rest) = rest.split_once(':')?;
   let digits = rest
      .find(|c: char| !c.is_ascii_digit())
      .unwrap_or(rest.len());
   Some((line.parse().ok()?, rest[..digits].parse().ok()?))
}

/// The engine's `line` and byte `column` in the wrapped function, as a place
/// in `source`.
fn at(source: &str, line: usize, column: usize) -> At {
   let Some(line) = line.checked_sub(WRAPPER_LINES).filter(|&line| line > 0) else {
      return At::Nowhere;
   };
   source.split('\n').nth(line - 1).map_or(At::End, |text| {
      At::Line {
         line,
         column: char_column(text, column),
      }
   })
}

/// `QuickJS` counts columns in bytes, people count characters.
fn char_column(text: &str, byte_column: usize) -> usize {
   let mut end = byte_column.saturating_sub(1).min(text.len());
   while !text.is_char_boundary(end) {
      end -= 1;
   }
   text[..end].chars().count() + 1
}

#[cfg(test)]
mod tests {
   use rquickjs::{
      CatchResultExt,
      Context,
      Runtime,
      Undefined,
   };

   use super::*;

   /// What compiling `source` and calling it as a plugin throws.
   fn thrown(source: &str) -> Option<SourceError> {
      let runtime = Runtime::new().unwrap();
      let context = Context::full(&runtime).unwrap();
      context.with(|ctx| {
         crate::js::compile(&ctx, source)
            .and_then(|start| start.call::<_, ()>((Undefined,)))
            .catch(&ctx)
            .map_err(|err| SourceError::caught(&err, source))
            .err()
      })
   }

   #[test]
   fn a_syntax_error_points_at_its_line_in_the_source() {
      let error = thrown("// @name p\nconst a = 1;\nconst b = ;\n").unwrap();
      assert_eq!(
         error.message,
         "SyntaxError: unexpected token in expression: ';'"
      );
      assert_eq!(error.at, At::Line {
         line:   3,
         column: 11,
      });
      assert_eq!(
         error.to_string(),
         "SyntaxError: unexpected token in expression: ';' (line 3, column 11)"
      );
   }

   #[test]
   fn columns_count_characters() {
      let error = thrown("const s = '\u{e9}'; const b = ;").unwrap();
      assert_eq!(error.at, At::Line {
         line:   1,
         column: 26,
      });
   }

   #[test]
   fn a_missing_brace_points_past_the_last_line() {
      let error = thrown("function f() {\n  return 1;\n").unwrap();
      assert!(error.message.starts_with("SyntaxError: "), "{error}");
      assert_eq!(error.at, At::End);
   }

   #[test]
   fn an_error_while_starting_points_at_the_plugin_line() {
      let error = thrown("const a = 1;\nfunction f() {\n  return missing + a;\n}\nf();").unwrap();
      assert_eq!(error.message, "ReferenceError: missing is not defined");
      assert_eq!(error.at, At::Line {
         line:   3,
         column: 10,
      });
   }

   #[test]
   fn a_thrown_value_has_no_place() {
      let error = thrown("throw 'plain';").unwrap();
      assert_eq!(error.at, At::Nowhere);
      assert!(error.message.contains("plain"), "{error}");
   }

   #[test]
   fn code_that_runs_has_no_error() {
      assert_eq!(thrown("const a = 1;\n(() => a)();"), None);
   }
}
