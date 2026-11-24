//! Android's document picker and opening links, through `phx.Picker`
//! (`java/phx/Picker.java`), loaded from an in-memory dex under Phoenix's class
//! loader. The chosen file arrives on a Java thread and goes to
//! [`crate::settings::picked`].

use std::{
   collections::HashMap,
   ffi::{
      CString,
      c_void,
   },
   ptr,
   sync::{
      Mutex,
      OnceLock,
      PoisonError,
      atomic::{
         AtomicI32,
         Ordering,
      },
      mpsc::{
         self,
         Sender,
      },
   },
};

use jni_sys::{
   JNIEnv,
   JNINativeMethod,
   jbyteArray,
   jclass,
   jint,
   jlong,
   jmethodID,
   jobject,
   jstring,
   jvalue,
};

use crate::{
   bridge,
   jvm::{
      self,
      jni,
   },
};

const DEX: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/picker.dex"));
/// Any type: pickers often file `.js` under an unexpected MIME type.
const MIME: &core::ffi::CStr = c"*/*";

/// The file the user chose, by name, or `None` when they backed out.
pub type Picked = Result<Option<(String, Vec<u8>)>, String>;

/// `phx.Picker` as a global reference.
static CLASS: OnceLock<usize> = OnceLock::new();
/// `phx.Updater` as a global reference (from the same in-memory dex).
static UPDATER_CLASS: OnceLock<usize> = OnceLock::new();
/// `phx.EmojiDraw` as a global reference (from the same in-memory dex).
static EMOJI_DRAW_CLASS: OnceLock<usize> = OnceLock::new();
/// Who waits for each open [`choose`] dialog.
static CHOICES: Mutex<Option<HashMap<i32, Sender<i32>>>> = Mutex::new(None);

/// Shows a dialog over Phoenix's activity and waits for it: a list of
/// `options` without `message`, or `message` with up to three buttons.
/// Returns the chosen option's index, or -1 when it was dismissed.
pub fn choose(
   title: Option<&str>,
   message: Option<&str>,
   options: &[String],
) -> Result<i32, String> {
   static NEXT: AtomicI32 = AtomicI32::new(1);
   let id = NEXT.fetch_add(1, Ordering::Relaxed);
   let (sender, receiver) = mpsc::channel();
   CHOICES
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .get_or_insert_default()
      .insert(id, sender);
   let shown = call_static(c"choose", |env| {
      // SAFETY: `env` is this thread's, inside the caller's local frame.
      unsafe {
         let string_class = jni!(env, FindClass, c"java/lang/String".as_ptr());
         let array = jni!(
            env,
            NewObjectArray,
            jni_sys::jsize::try_from(options.len()).unwrap_or_default(),
            string_class,
            ptr::null_mut()
         );
         for (index, option) in options.iter().enumerate() {
            let option = jstring_of(env, option);
            jni!(
               env,
               SetObjectArrayElement,
               array,
               jni_sys::jsize::try_from(index).unwrap_or_default(),
               option
            );
         }
         (
            c"(Ljava/lang/Object;ILjava/lang/String;Ljava/lang/String;[Ljava/lang/String;)V",
            vec![
               jvalue {
                  l: bridge::guest_host(),
               },
               jvalue { i: id },
               jvalue {
                  l: title.map_or(ptr::null_mut(), |title| jstring_of(env, title)),
               },
               jvalue {
                  l: message.map_or(ptr::null_mut(), |message| jstring_of(env, message)),
               },
               jvalue { l: array },
            ],
         )
      }
   });
   let forget = || {
      if let Some(choices) = CHOICES
         .lock()
         .unwrap_or_else(PoisonError::into_inner)
         .as_mut()
      {
         choices.remove(&id);
      }
   };
   if let Err(err) = shown {
      forget();
      return Err(err);
   }
   let chosen = receiver.recv().unwrap_or(-1);
   forget();
   Ok(chosen)
}

/// Shows `text` briefly at the bottom of the screen.
pub fn toast(text: &str) -> Result<(), String> {
   call_static(c"toast", |env| {
      // SAFETY: as in `choose`.
      unsafe {
         (c"(Ljava/lang/Object;Ljava/lang/String;)V", vec![
            jvalue {
               l: bridge::guest_host(),
            },
            jvalue {
               l: jstring_of(env, text),
            },
         ])
      }
   })
}

/// Copies `text` to the Android clipboard.
pub fn copy(text: &str) -> Result<(), String> {
   call_static(c"copy", |env| {
      // SAFETY: as in `choose`.
      unsafe {
         (c"(Ljava/lang/Object;Ljava/lang/String;)V", vec![
            jvalue {
               l: bridge::guest_host(),
            },
            jvalue {
               l: jstring_of(env, text),
            },
         ])
      }
   })
}

/// Shows a scrollable selectable text/code dialog over Phoenix's activity and
/// waits for a button press (`0..options.len()`) or dismiss (-1).
pub fn view(
   title: Option<&str>,
   text: &str,
   monospace: bool,
   options: &[String],
) -> Result<i32, String> {
   static NEXT: AtomicI32 = AtomicI32::new(100_000);
   let id = NEXT.fetch_add(1, Ordering::Relaxed);
   let (sender, receiver) = mpsc::channel();
   CHOICES
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .get_or_insert_default()
      .insert(id, sender);
   let shown = call_static(c"view", |env| {
      // SAFETY: `env` is this thread's, inside the caller's local frame.
      unsafe {
         let string_class = jni!(env, FindClass, c"java/lang/String".as_ptr());
         let array = jni!(
            env,
            NewObjectArray,
            jni_sys::jsize::try_from(options.len()).unwrap_or_default(),
            string_class,
            ptr::null_mut()
         );
         for (index, option) in options.iter().enumerate() {
            let option = jstring_of(env, option);
            jni!(
               env,
               SetObjectArrayElement,
               array,
               jni_sys::jsize::try_from(index).unwrap_or_default(),
               option
            );
         }
         (
            c"(Ljava/lang/Object;ILjava/lang/String;Ljava/lang/String;Z[Ljava/lang/String;)V",
            vec![
               jvalue {
                  l: bridge::guest_host(),
               },
               jvalue { i: id },
               jvalue {
                  l: title.map_or(ptr::null_mut(), |title| jstring_of(env, title)),
               },
               jvalue {
                  l: jstring_of(env, text),
               },
               jvalue { z: monospace },
               jvalue { l: array },
            ],
         )
      }
   });
   let forget = || {
      if let Some(choices) = CHOICES
         .lock()
         .unwrap_or_else(PoisonError::into_inner)
         .as_mut()
      {
         choices.remove(&id);
      }
   };
   if let Err(err) = shown {
      forget();
      return Err(err);
   }
   let chosen = receiver.recv().unwrap_or(-1);
   forget();
   Ok(chosen)
}

/// A Java string of `text`, with NULs dropped.
unsafe fn jstring_of(env: *mut JNIEnv, text: &str) -> jobject {
   let text = CString::new(text.replace('\0', "")).unwrap_or_default();
   // SAFETY: forwarded from the caller.
   unsafe { jni!(env, NewStringUTF, text.as_ptr()) }
}

/// Calls the static `phx.Picker.name` with the signature and arguments `args`
/// makes, in a local frame.
fn call_static(
   name: &core::ffi::CStr,
   args: impl FnOnce(*mut JNIEnv) -> (&'static core::ffi::CStr, Vec<jvalue>),
) -> Result<(), String> {
   let env = jvm::env().ok_or("no JNI env")?;
   // SAFETY: as in `pick`; every reference made here is local to the frame.
   unsafe {
      if jni!(env, PushLocalFrame, 32) != 0 {
         return Err(failure(env, "out of local references"));
      }
      let result = (|| {
         let class = class(env)?;
         let (signature, args) = args(env);
         let method = jni!(
            env,
            GetStaticMethodID,
            class,
            name.as_ptr(),
            signature.as_ptr()
         );
         if method.is_null() {
            return Err(failure(
               env,
               &format!("phx.Picker.{} is missing", name.to_string_lossy()),
            ));
         }
         jni!(env, CallStaticVoidMethodA, class, method, args.as_ptr());
         if jni!(env, ExceptionCheck) {
            return Err(failure(
               env,
               &format!("phx.Picker.{} threw", name.to_string_lossy()),
            ));
         }
         Ok(())
      })();
      jni!(env, PopLocalFrame, ptr::null_mut());
      result
   }
}

/// Opens the picker over Phoenix's activity. The answer comes later through
/// [`crate::settings::picked`].
pub fn pick() -> Result<(), String> {
   let env = jvm::env().ok_or("no JNI env")?;
   // SAFETY: `env` is this thread's, and every reference made here is local
   // to the frame popped below, except the class kept as a global reference.
   unsafe {
      if jni!(env, PushLocalFrame, 16) != 0 {
         return Err(failure(env, "out of local references"));
      }
      let result = call_pick(env);
      jni!(env, PopLocalFrame, ptr::null_mut());
      result
   }
}

/// Opens `url` in the browser over Phoenix's activity.
pub fn open(url: &str) -> Result<(), String> {
   let env = jvm::env().ok_or("no JNI env")?;
   let url = std::ffi::CString::new(url).map_err(|err| err.to_string())?;
   // SAFETY: as in `pick`.
   unsafe {
      if jni!(env, PushLocalFrame, 8) != 0 {
         return Err(failure(env, "out of local references"));
      }
      let result = call_open(env, &url);
      jni!(env, PopLocalFrame, ptr::null_mut());
      result
   }
}

/// Repacks the currently installed APK with `new_loader`, signs it with APK
/// Signature Scheme v3, and opens Android's `PackageInstaller` prompt.
pub fn self_update(
   new_loader: &[u8],
   pkcs8: &str,
   spki: &str,
   cert: &str,
   lineage: &str,
   fallback_url: &str,
) -> Result<(), String> {
   let env = jvm::env().ok_or("no JNI env")?;
   // SAFETY: `env` belongs to this thread and all local refs stay inside the
   // frame.
   unsafe {
      if jni!(env, PushLocalFrame, 32) != 0 {
         return Err(failure(env, "out of local references"));
      }
      let result = (|| {
         let _ = class(env)?;
         let updater = updater_class()?;
         let method = jni!(
            env,
            GetStaticMethodID,
            updater,
            c"selfUpdate".as_ptr(),
            c"(Ljava/lang/Object;[BLjava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)V".as_ptr(),
         );
         if method.is_null() {
            return Err(failure(env, "phx.Updater.selfUpdate is missing"));
         }
         let Some((loader_arr, _)) = jvm::new_bytes(env, new_loader) else {
            return Err("could not allocate loader byte array".to_owned());
         };
         let args = [
            jvalue {
               l: bridge::guest_host(),
            },
            jvalue { l: loader_arr },
            jvalue {
               l: jstring_of(env, pkcs8),
            },
            jvalue {
               l: jstring_of(env, spki),
            },
            jvalue {
               l: jstring_of(env, cert),
            },
            jvalue {
               l: jstring_of(env, lineage),
            },
            jvalue {
               l: jstring_of(env, fallback_url),
            },
         ];
         jni!(env, CallStaticVoidMethodA, updater, method, args.as_ptr());
         if jni!(env, ExceptionCheck) {
            return Err(failure(env, "phx.Updater.selfUpdate threw"));
         }
         Ok(())
      })();
      jni!(env, PopLocalFrame, ptr::null_mut());
      result
   }
}

fn updater_class() -> Result<jclass, String> {
   UPDATER_CLASS
      .get()
      .copied()
      .map(|ptr| ptr as jclass)
      .ok_or_else(|| "phx.Updater class not loaded".to_owned())
}

unsafe fn call_open(env: *mut JNIEnv, url: &core::ffi::CStr) -> Result<(), String> {
   // SAFETY: forwarded from `open`.
   unsafe {
      let class = class(env)?;
      let method = jni!(
         env,
         GetStaticMethodID,
         class,
         c"open".as_ptr(),
         c"(Ljava/lang/Object;Ljava/lang/String;)V".as_ptr(),
      );
      if method.is_null() {
         return Err(failure(env, "phx.Picker.open is missing"));
      }
      let args = [
         jvalue {
            l: bridge::guest_host(),
         },
         jvalue {
            l: jni!(env, NewStringUTF, url.as_ptr()),
         },
      ];
      jni!(env, CallStaticVoidMethodA, class, method, args.as_ptr());
      if jni!(env, ExceptionCheck) {
         return Err(failure(env, "phx.Picker.open threw"));
      }
      Ok(())
   }
}

unsafe fn call_pick(env: *mut JNIEnv) -> Result<(), String> {
   // SAFETY: forwarded from `pick`.
   unsafe {
      let class = class(env)?;
      let method = jni!(
         env,
         GetStaticMethodID,
         class,
         c"pick".as_ptr(),
         c"(Ljava/lang/Object;ILjava/lang/String;)V".as_ptr(),
      );
      if method.is_null() {
         return Err(failure(env, "phx.Picker.pick is missing"));
      }
      let mime = jni!(env, NewStringUTF, MIME.as_ptr());
      let args = [
         jvalue {
            l: bridge::guest_host(),
         },
         jvalue { i: 0 },
         jvalue { l: mime },
      ];
      jni!(env, CallStaticVoidMethodA, class, method, args.as_ptr());
      if jni!(env, ExceptionCheck) {
         return Err(failure(env, "phx.Picker.pick threw"));
      }
      Ok(())
   }
}

/// Loads `phx.Updater` from the dex's `loader` and binds its status
/// callback. The updater is optional, so a failure only leaves it unbound.
unsafe fn bind_updater(env: *mut JNIEnv, loader: jobject, load: jmethodID) {
   // SAFETY: forwarded from `class`, which holds live `loader` and `load`.
   unsafe {
      let name = jni!(env, NewStringUTF, c"phx.Updater".as_ptr());
      let updater = jni!(
         env,
         CallObjectMethodA,
         loader,
         load,
         [jvalue { l: name }].as_ptr()
      );
      if updater.is_null() || jni!(env, ExceptionCheck) {
         let _ = jvm::take_exception(env);
         return;
      }
      let native = JNINativeMethod {
         name:      c"onUpdateStatus".as_ptr().cast_mut(),
         signature: c"(Ljava/lang/String;Ljava/lang/String;)V"
            .as_ptr()
            .cast_mut(),
         fnPtr:     on_update_status as *mut c_void,
      };
      if jni!(env, RegisterNatives, updater, &raw const native, 1) != 0 {
         let _ = jvm::take_exception(env);
         return;
      }
      let global = jni!(env, NewGlobalRef, updater) as usize;
      let _ = UPDATER_CLASS.set(global);
   }
}

/// Loads `phx.EmojiDraw` from the dex's `loader`.
unsafe fn bind_emoji_draw(env: *mut JNIEnv, loader: jobject, load: jmethodID) {
   // SAFETY: forwarded from `class`, which holds live `loader` and `load`.
   unsafe {
      let name = jni!(env, NewStringUTF, c"phx.EmojiDraw".as_ptr());
      let cls = jni!(
         env,
         CallObjectMethodA,
         loader,
         load,
         [jvalue { l: name }].as_ptr()
      );
      if cls.is_null() || jni!(env, ExceptionCheck) {
         let _ = jvm::take_exception(env);
         return;
      }
      let global = jni!(env, NewGlobalRef, cls) as usize;
      let _ = EMOJI_DRAW_CLASS.set(global);
   }
}

/// Renders a Unicode emoji sequence (dash-separated hex codepoints) to `out_path`
/// using `"system"` or a custom font file path, via `phx.EmojiDraw.draw`.
pub(crate) fn draw_emoji(codepoints: &str, font: &str, out_path: &str) -> Option<String> {
   let env = jvm::env()?;
   // SAFETY: `env` is this thread's JNIEnv, and PushLocalFrame frees all local refs.
   unsafe {
      if jni!(env, PushLocalFrame, 32) != 0 {
         jvm::clear_exception(env);
         return None;
      }
      let result = (|| {
         if EMOJI_DRAW_CLASS.get().is_none() {
            let _ = class(env);
         }
         let cls = (*EMOJI_DRAW_CLASS.get()?) as jclass;
         let method = jni!(
            env,
            GetStaticMethodID,
            cls,
            c"draw".as_ptr(),
            c"(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;".as_ptr()
         );
         if method.is_null() || jni!(env, ExceptionCheck) {
            jvm::clear_exception(env);
            return None;
         }
         let args = [
            jvalue {
               l: jstring_of(env, codepoints),
            },
            jvalue {
               l: jstring_of(env, font),
            },
            jvalue {
               l: jstring_of(env, out_path),
            },
         ];
         let out = jni!(env, CallStaticObjectMethodA, cls, method, args.as_ptr());
         if out.is_null() || jni!(env, ExceptionCheck) {
            jvm::clear_exception(env);
            return None;
         }
         jvm::read_string(env, out)
      })();
      jni!(env, PopLocalFrame, ptr::null_mut());
      result
   }
}

/// Loads `phx.Picker` once and binds its native callback.
unsafe fn class(env: *mut JNIEnv) -> Result<jclass, String> {
   if let Some(&class) = CLASS.get() {
      return Ok(class as jclass);
   }
   let owner = bridge::guest_host();
   if owner.is_null() {
      return Err("Phoenix's UI bridge is not up yet".to_owned());
   }
   // SAFETY: forwarded from `pick`; `owner` is the live GuestHost global
   // reference and `DEX` is static, so the direct buffer never dangles.
   unsafe {
      let class_class = jni!(env, FindClass, c"java/lang/Class".as_ptr());
      let get_loader = jvm::method(
         env,
         class_class,
         c"getClassLoader",
         c"()Ljava/lang/ClassLoader;",
      );
      let owner_class = jni!(env, GetObjectClass, owner);
      let parent = jni!(env, CallObjectMethodA, owner_class, get_loader, ptr::null());
      let buffer = jni!(
         env,
         NewDirectByteBuffer,
         DEX.as_ptr().cast_mut().cast::<c_void>(),
         jlong::try_from(DEX.len()).expect("the picker dex is a few KB"),
      );
      let loader_class = jni!(
         env,
         FindClass,
         c"dalvik/system/InMemoryDexClassLoader".as_ptr()
      );
      let init = jvm::method(
         env,
         loader_class,
         c"<init>",
         c"(Ljava/nio/ByteBuffer;Ljava/lang/ClassLoader;)V",
      );
      let args = [jvalue { l: buffer }, jvalue { l: parent }];
      let loader = jni!(env, NewObjectA, loader_class, init, args.as_ptr());
      if loader.is_null() || jni!(env, ExceptionCheck) {
         return Err(failure(env, "could not load the picker dex"));
      }
      let base = jni!(env, FindClass, c"java/lang/ClassLoader".as_ptr());
      let load = jvm::method(
         env,
         base,
         c"loadClass",
         c"(Ljava/lang/String;)Ljava/lang/Class;",
      );
      let name = jni!(env, NewStringUTF, c"phx.Picker".as_ptr());
      let class = jni!(
         env,
         CallObjectMethodA,
         loader,
         load,
         [jvalue { l: name }].as_ptr()
      );
      if class.is_null() || jni!(env, ExceptionCheck) {
         return Err(failure(env, "phx.Picker is not in the dex"));
      }
      let natives = [
         JNINativeMethod {
            name:      c"onPicked".as_ptr().cast_mut(),
            signature: c"(ILjava/lang/String;[BLjava/lang/String;)V"
               .as_ptr()
               .cast_mut(),
            fnPtr:     on_picked as *mut c_void,
         },
         JNINativeMethod {
            name:      c"onChosen".as_ptr().cast_mut(),
            signature: c"(II)V".as_ptr().cast_mut(),
            fnPtr:     on_chosen as *mut c_void,
         },
      ];
      if jni!(env, RegisterNatives, class, natives.as_ptr(), 2) != 0 {
         return Err(failure(env, "could not bind phx.Picker's natives"));
      }
      bind_updater(env, loader, load);
      bind_emoji_draw(env, loader, load);
      let global = jni!(env, NewGlobalRef, class) as usize;
      Ok(*CLASS.get_or_init(|| global) as jclass)
   }
}

/// The pending exception's text, or `what` when there is none.
unsafe fn failure(env: *mut JNIEnv, what: &str) -> String {
   // SAFETY: forwarded from the caller.
   unsafe { jvm::take_exception(env) }
      .map_or_else(|| what.to_owned(), |thrown| format!("{what}: {thrown}"))
}

unsafe extern "system" fn on_picked(
   env: *mut JNIEnv,
   _class: jclass,
   _id: jint,
   name: jstring,
   data: jbyteArray,
   error: jstring,
) {
   // SAFETY: Java passes live references or null, on its own attached thread.
   let picked = unsafe {
      jvm::read_string(env, error).map_or_else(
         || Ok(jvm::read_string(env, name).map(|name| (name, jvm::read_bytes(env, data, None)))),
         Err,
      )
   };
   crate::settings::picked(picked);
}

unsafe extern "system" fn on_chosen(_env: *mut JNIEnv, _class: jclass, id: jint, index: jint) {
   let waiter = CHOICES
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .as_mut()
      .and_then(|choices| choices.remove(&id));
   if let Some(waiter) = waiter {
      let _ = waiter.send(index);
   }
}

unsafe extern "system" fn on_update_status(
   env: *mut JNIEnv,
   _class: jclass,
   state: jstring,
   message: jstring,
) {
   // SAFETY: Java passes live strings or null on its attached thread.
   let (state, message) = unsafe {
      (
         jvm::read_string(env, state).unwrap_or_default(),
         jvm::read_string(env, message).unwrap_or_default(),
      )
   };
   match state.parse() {
      Ok(known) => crate::update::on_native_status(known, &message),
      Err(err) => log::warn!("self-update: {err}"),
   }
}
