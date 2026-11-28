//! `phoenix.fetch` and `phoenix.discord`.

use std::sync::Arc;

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

use crate::{
   discord,
   fetch,
   js::{
      background::{
         Settlement,
         in_background,
      },
      intercept::add_discord_hooks,
      state,
      web::bytes_of,
   },
   permissions::{
      Permission,
      Permissions,
   },
   store,
};

/// `{status, ok, text(), json(), arrayBuffer(), bytes()}`, a subset of the
/// web's `Response`. The body is read on each call, as text or as the raw
/// bytes images and files need.
pub fn response_object<'js>(
   ctx: &Ctx<'js>,
   response: &fetch::Response,
) -> rquickjs::Result<Object<'js>> {
   let object = Object::new(ctx.clone())?;
   object.set("status", response.status)?;
   object.set("ok", (200..300).contains(&response.status))?;
   let body: Arc<[u8]> = Arc::from(response.body.as_slice());
   let text = Arc::clone(&body);
   object.set(
      "text",
      Function::new(ctx.clone(), move || {
         String::from_utf8_lossy(&text).into_owned()
      })?,
   )?;
   let json = Arc::clone(&body);
   object.set(
      "json",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>| {
         ctx.json_parse(String::from_utf8_lossy(&json).into_owned())
      })?,
   )?;
   let buffer = Arc::clone(&body);
   object.set(
      "arrayBuffer",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>| {
         ArrayBuffer::new_copy(ctx, &*buffer)
      })?,
   )?;
   object.set(
      "bytes",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>| {
         TypedArray::<u8>::new_copy(ctx, &*body)
      })?,
   )?;
   Ok(object)
}

/// A fetch-style `{method, headers, body}`. A string body is sent as it is, an
/// `ArrayBuffer` or a view of one as its bytes, and anything else as JSON.
struct Init {
   method:  String,
   headers: Vec<(String, String)>,
   body:    Option<Vec<u8>>,
}

impl Init {
   fn read<'js>(ctx: &Ctx<'js>, init: Option<Object<'js>>) -> rquickjs::Result<Self> {
      let mut out = Self {
         method:  "GET".to_owned(),
         headers: Vec::new(),
         body:    None,
      };
      let Some(init) = init else {
         return Ok(out);
      };
      if let Some(method) = init.get::<_, Option<String>>("method")? {
         out.method = method.to_ascii_uppercase();
      }
      if let Some(headers) = init.get::<_, Option<Object<'js>>>("headers")? {
         out.headers = headers
            .props::<String, String>()
            .collect::<rquickjs::Result<_>>()?;
      }
      out.set_body(ctx, init.get("body")?)?;
      Ok(out)
   }

   fn set_body<'js>(&mut self, ctx: &Ctx<'js>, body: Value<'js>) -> rquickjs::Result<()> {
      if body.is_undefined() || body.is_null() {
         return Ok(());
      }
      if let Some(text) = body.as_string() {
         self.body = Some(text.to_string()?.into_bytes());
         return Ok(());
      }
      if let Some(bytes) = bytes_of(&body)? {
         self.body = Some(bytes);
         return Ok(());
      }
      if !self
         .headers
         .iter()
         .any(|(name, _)| name.eq_ignore_ascii_case("content-type"))
      {
         self
            .headers
            .push(("Content-Type".to_owned(), "application/json".to_owned()));
      }
      self.body = ctx
         .json_stringify(body)?
         .map(|json| json.to_string())
         .transpose()?
         .map(String::into_bytes);
      Ok(())
   }
}

/// Sends the request on its own thread, so neither the gateway nor the UI
/// waits on the network, and returns a promise that settles when it answers.
pub fn start_fetch<'js>(
   ctx: &Ctx<'js>,
   url: String,
   init: Option<Object<'js>>,
) -> rquickjs::Result<rquickjs::Promise<'js>> {
   let Init {
      method,
      headers,
      body,
   } = Init::read(ctx, init)?;
   let request = fetch::Request {
      url,
      method,
      headers,
      body,
   };
   in_background(ctx, move || Settlement::Fetch(fetch::send(&request)))
}

fn send<'js>(
   ctx: &Ctx<'js>,
   permissions: &Permissions,
   path: String,
   init: Init,
) -> rquickjs::Result<rquickjs::Promise<'js>> {
   allow(ctx, permissions, &init.method, &path)?;
   in_background(ctx, move || {
      Settlement::Discord(discord::send(
         &init.method,
         &path,
         init.headers,
         init.body.unwrap_or_default(),
      ))
   })
}

fn allow(
   ctx: &Ctx<'_>,
   permissions: &Permissions,
   method: &str,
   path: &str,
) -> rquickjs::Result<()> {
   if permissions.allows(Permission::Discord) {
      return Ok(());
   }
   Err(Exception::throw_message(
      ctx,
      &format!("{method} {path}: add discord to the plugin's // @permissions line"),
   ))
}

/// `phoenix.discord`, Discord's API on the app's own client and session:
/// `request(path, init)`, `get` and `delete(path, init)`, `post`, `put` and
/// `patch(path, body, init)`, and `onRequest(filter, handler)`.
pub fn discord_object<'js>(
   ctx: &Ctx<'js>,
   plugin: &str,
   permissions: &Arc<Permissions>,
) -> rquickjs::Result<Object<'js>> {
   let object = Object::new(ctx.clone())?;
   add_discord_hooks(ctx, plugin, &object, permissions)?;
   let granted = Arc::clone(permissions);
   object.set(
      "authorize",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>, options: Object<'js>| {
         allow(&ctx, &granted, "POST", "/oauth2/authorize")?;
         let client_id = options.get::<_, Option<String>>("clientId")?;
         let redirect_uri = options.get::<_, Option<String>>("redirectUri")?;
         let (Some(client_id), Some(redirect_uri)) = (client_id, redirect_uri) else {
            return Err(Exception::throw_type(
               &ctx,
               "authorize needs clientId and redirectUri",
            ));
         };
         let request = discord::Authorization {
            client_id,
            redirect_uri,
            scopes: options
               .get::<_, Option<Vec<String>>>("scopes")?
               .unwrap_or_else(|| vec!["identify".to_owned()]),
            response_type: options
               .get::<_, Option<String>>("responseType")?
               .unwrap_or_else(|| "code".to_owned()),
         };
         in_background(&ctx, move || Settlement::Text(discord::authorize(&request)))
      })?,
   )?;
   let granted = Arc::clone(permissions);
   object.set(
      "me",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>| {
         allow(&ctx, &granted, "GET", "/users/@me")?;
         let files = state().files_dir();
         in_background(&ctx, move || {
            Settlement::Json(store::database(&files).and_then(|db| store::meta(&db, "me")))
         })
      })?,
   )?;
   let granted = Arc::clone(permissions);
   object.set(
      "request",
      Function::new(
         ctx.clone(),
         move |ctx: Ctx<'js>, path: String, init: Opt<Object<'js>>| {
            send(&ctx, &granted, path, Init::read(&ctx, init.0)?)
         },
      )?,
   )?;
   for verb in ["GET", "DELETE"] {
      let granted = Arc::clone(permissions);
      object.set(
         verb.to_ascii_lowercase(),
         Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, path: String, init: Opt<Object<'js>>| {
               let mut init = Init::read(&ctx, init.0)?;
               verb.clone_into(&mut init.method);
               send(&ctx, &granted, path, init)
            },
         )?,
      )?;
   }
   for verb in ["POST", "PUT", "PATCH"] {
      let granted = Arc::clone(permissions);
      object.set(
         verb.to_ascii_lowercase(),
         Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, path: String, body: Opt<Value<'js>>, init: Opt<Object<'js>>| {
               let mut init = Init::read(&ctx, init.0)?;
               verb.clone_into(&mut init.method);
               if let Some(body) = body.0 {
                  init.set_body(&ctx, body)?;
               }
               send(&ctx, &granted, path, init)
            },
         )?,
      )?;
   }
   Ok(object)
}

#[cfg(test)]
mod tests {
   use rquickjs::{
      Context,
      Runtime,
   };

   use super::*;

   fn body_of(code: &str) -> (Option<Vec<u8>>, Vec<(String, String)>) {
      let runtime = Runtime::new().unwrap();
      let context = Context::full(&runtime).unwrap();
      context.with(|ctx| {
         let init: Object<'_> = ctx.eval(code).unwrap();
         let init = Init::read(&ctx, Some(init)).unwrap();
         (init.body, init.headers)
      })
   }

   #[test]
   fn binary_bodies_are_sent_as_their_bytes() {
      let (body, headers) = body_of("({ method: 'PUT', body: new Uint8Array([0, 255, 7]) })");
      assert_eq!(body.as_deref(), Some(&[0, 255, 7][..]));
      assert_eq!(headers, Vec::<(String, String)>::new());
      let (body, _) = body_of("({ body: new Uint16Array([1, 2]).buffer })");
      assert_eq!(body.map(|body| body.len()), Some(4));
      let (body, _) =
         body_of("({ body: new DataView(new Uint8Array([9, 8, 7, 6]).buffer, 1, 2) })");
      assert_eq!(body.as_deref(), Some(&[8, 7][..]));
      let (body, headers) = body_of("({ body: { a: 1 } })");
      assert_eq!(body.as_deref(), Some(&br#"{"a":1}"#[..]));
      assert_eq!(headers, [(
         "Content-Type".to_owned(),
         "application/json".to_owned()
      )]);
   }
}
