//! `TextEncoder`, `TextDecoder`, `crypto` and `console`, web globals that
//! plugins ported from other client mods use and `QuickJS` lacks.
//!
//! Each plugin gets them as names of its own, next to the timers, since the
//! shared globals are frozen and a shared `console` could not say whose line it
//! logs.

use rquickjs::{
   ArrayBuffer,
   Ctx,
   Exception,
   Function,
   Object,
   TypedArray,
   Value,
   function::Opt,
};

/// What `crypto.getRandomValues` fills at most in one call, as in browsers.
const RANDOM_LIMIT: usize = 65_536;

/// The integer views `crypto.getRandomValues` takes.
const INTEGER_VIEWS: [&str; 9] = [
   "Int8Array",
   "Uint8Array",
   "Uint8ClampedArray",
   "Int16Array",
   "Uint16Array",
   "Int32Array",
   "Uint32Array",
   "BigInt64Array",
   "BigUint64Array",
];

/// The bytes of an `ArrayBuffer` or of the part of one a view (a typed array
/// or `DataView`) covers, copied out. `None` for anything else.
pub fn bytes_of(value: &Value<'_>) -> rquickjs::Result<Option<Vec<u8>>> {
   if let Some(buffer) = ArrayBuffer::from_value(value.clone()) {
      return Ok(Some(copy(&buffer, 0, buffer.len())));
   }
   let Some(view) = value.as_object() else {
      return Ok(None);
   };
   let Some(buffer) = view
      .get::<_, Value<'_>>("buffer")
      .ok()
      .and_then(ArrayBuffer::from_value)
   else {
      return Ok(None);
   };
   let (Some(offset), Some(length)) = (
      view.get::<_, Option<f64>>("byteOffset")?,
      view.get::<_, Option<f64>>("byteLength")?,
   ) else {
      return Ok(None);
   };
   #[expect(
      clippy::cast_possible_truncation,
      clippy::cast_sign_loss,
      reason = "a view's offset and length are whole numbers within its buffer"
   )]
   Ok(Some(copy(&buffer, offset as usize, length as usize)))
}

fn copy(buffer: &ArrayBuffer<'_>, offset: usize, length: usize) -> Vec<u8> {
   // SAFETY: the bytes are copied out before any JS runs that could detach
   // or resize the buffer.
   unsafe { buffer.as_bytes() }
      .and_then(|bytes| bytes.get(offset..offset.checked_add(length)?))
      .map_or_else(Vec::new, <[u8]>::to_vec)
}

/// A function `new` may call, which returns the object it builds.
fn constructor(function: Function<'_>) -> Function<'_> {
   function.with_constructor(true)
}

/// `new TextEncoder()`, UTF-8 only like the web's.
fn text_encoder<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Function<'js>> {
   Ok(constructor(Function::new(ctx.clone(), |ctx: Ctx<'js>| {
      let encoder = Object::new(ctx.clone())?;
      encoder.set("encoding", "utf-8")?;
      encoder.set(
         "encode",
         Function::new(ctx.clone(), |ctx: Ctx<'js>, text: Opt<String>| {
            TypedArray::<u8>::new(ctx, text.0.unwrap_or_default().into_bytes())
         })?,
      )?;
      Ok::<_, rquickjs::Error>(encoder)
   })?))
}

/// `new TextDecoder(label, {fatal, ignoreBOM})`, UTF-8 only.
fn text_decoder<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Function<'js>> {
   Ok(constructor(Function::new(
      ctx.clone(),
      |ctx: Ctx<'js>, label: Opt<String>, options: Opt<Object<'js>>| {
         let label = label.0.unwrap_or_else(|| "utf-8".to_owned());
         if !["utf-8", "utf8", "unicode-1-1-utf-8"]
            .contains(&label.trim().to_ascii_lowercase().as_str())
         {
            return Err(Exception::throw_range(
               &ctx,
               &format!("the encoding {label} is not supported, only utf-8"),
            ));
         }
         let option = |name: &str| -> rquickjs::Result<bool> {
            Ok(options
               .0
               .as_ref()
               .map(|options| options.get::<_, Option<bool>>(name))
               .transpose()?
               .flatten()
               .unwrap_or(false))
         };
         let fatal = option("fatal")?;
         let keep_bom = option("ignoreBOM")?;
         let decoder = Object::new(ctx.clone())?;
         decoder.set("encoding", "utf-8")?;
         decoder.set("fatal", fatal)?;
         decoder.set("ignoreBOM", keep_bom)?;
         decoder.set(
            "decode",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>, input: Opt<Value<'js>>| {
               let bytes = match input.0 {
                  None => Vec::new(),
                  Some(value) if value.is_undefined() => Vec::new(),
                  Some(value) => {
                     bytes_of(&value)?.ok_or_else(|| {
                        Exception::throw_type(&ctx, "decode takes an ArrayBuffer or a view of one")
                     })?
                  },
               };
               let bytes = match bytes.strip_prefix(b"\xEF\xBB\xBF") {
                  Some(rest) if !keep_bom => rest,
                  _ => bytes.as_slice(),
               };
               if fatal {
                  return std::str::from_utf8(bytes)
                     .map(str::to_owned)
                     .map_err(|_| Exception::throw_type(&ctx, "the input is not valid UTF-8"));
               }
               Ok(String::from_utf8_lossy(bytes).into_owned())
            })?,
         )?;
         Ok(decoder)
      },
   )?))
}

fn random(ctx: &Ctx<'_>, length: usize) -> rquickjs::Result<Vec<u8>> {
   let mut bytes = vec![0; length];
   getrandom::fill(&mut bytes).map_err(|err| {
      Exception::throw_internal(ctx, &format!("the system's random source failed: {err}"))
   })?;
   Ok(bytes)
}

/// `crypto.getRandomValues(view)` and `crypto.randomUUID()`, from the
/// system's random source.
fn crypto<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
   let crypto = Object::new(ctx.clone())?;
   crypto.set(
      "getRandomValues",
      Function::new(ctx.clone(), |ctx: Ctx<'js>, view: Value<'js>| {
         let kind = view
            .as_object()
            .map(|view| view.get::<_, Object<'js>>("constructor"))
            .transpose()
            .ok()
            .flatten()
            .map(|constructor| constructor.get::<_, String>("name"))
            .transpose()?;
         if !kind.is_some_and(|kind| INTEGER_VIEWS.contains(&kind.as_str())) {
            return Err(Exception::throw_type(
               &ctx,
               "getRandomValues takes an integer typed array",
            ));
         }
         let Some(object) = view.as_object() else {
            return Err(Exception::throw_type(
               &ctx,
               "getRandomValues takes an integer typed array",
            ));
         };
         #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a view's byte length is a whole number"
         )]
         let length = object.get::<_, f64>("byteLength")? as usize;
         if length > RANDOM_LIMIT {
            return Err(Exception::throw_range(
               &ctx,
               &format!("getRandomValues fills at most {RANDOM_LIMIT} bytes"),
            ));
         }
         // Written through a byte view of the same memory, so every integer
         // view gets filled the same way.
         let bytes = ctx
            .globals()
            .get::<_, rquickjs::function::Constructor<'js>>("Uint8Array")?
            .construct::<_, Object<'js>>((
               object.get::<_, Value<'js>>("buffer")?,
               object.get::<_, Value<'js>>("byteOffset")?,
               length,
            ))?;
         bytes.get::<_, Function<'js>>("set")?.call::<_, ()>((
            rquickjs::function::This(bytes.clone()),
            TypedArray::<u8>::new(ctx.clone(), random(&ctx, length)?)?,
         ))?;
         Ok(view)
      })?,
   )?;
   crypto.set(
      "randomUUID",
      Function::new(ctx.clone(), |ctx: Ctx<'js>| {
         let mut bytes = random(&ctx, 16)?;
         bytes[6] = (bytes[6] & 0x0F) | 0x40;
         bytes[8] = (bytes[8] & 0x3F) | 0x80;
         let hex = bytes
            .iter()
            .fold(String::with_capacity(32), |mut hex, byte| {
               use std::fmt::Write as _;
               let _ = write!(hex, "{byte:02x}");
               hex
            });
         Ok::<_, rquickjs::Error>(format!(
            "{}-{}-{}-{}-{}",
            &hex[..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..]
         ))
      })?,
   )?;
   Ok(crypto)
}

/// `console`, whose `log`, `info`, `debug`, `warn` and `error` write the
/// plugin's `phoenix.log` lines, and `trace` a debug one.
fn console<'js>(ctx: &Ctx<'js>, log: &Object<'js>) -> rquickjs::Result<Object<'js>> {
   let console = Object::new(ctx.clone())?;
   for (name, level) in [
      ("log", "info"),
      ("info", "info"),
      ("debug", "debug"),
      ("trace", "debug"),
      ("warn", "warn"),
      ("error", "error"),
   ] {
      console.set(name, log.get::<_, Function<'js>>(level)?)?;
   }
   Ok(console)
}

/// Adds `TextEncoder`, `TextDecoder`, `crypto` and `console` to `names`, the
/// last writing through `log`, the plugin's `phoenix.log`.
pub fn add_web<'js>(
   ctx: &Ctx<'js>,
   names: &Object<'js>,
   log: &Object<'js>,
) -> rquickjs::Result<()> {
   names.set("TextEncoder", text_encoder(ctx)?)?;
   names.set("TextDecoder", text_decoder(ctx)?)?;
   names.set("crypto", crypto(ctx)?)?;
   names.set("console", console(ctx, log)?)?;
   Ok(())
}
