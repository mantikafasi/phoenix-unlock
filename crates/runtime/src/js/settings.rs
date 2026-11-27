//! `phoenix.settings`, the settings a plugin defines and what the user chose.

use std::collections::HashSet;

use rquickjs::{
   CatchResultExt,
   Ctx,
   Exception,
   Function,
   IntoJs,
   Object,
   Value,
   object::Accessor,
};

use crate::{
   Choice,
   Chosen,
   Control,
   js::{
      Engine,
      Registry,
      restart_budget,
   },
};

/// Upper bound on fixed-point passes when dynamic select options depend on
/// another dynamic select.
const MAX_RESOLVE_PASSES: usize = 4;

impl Engine {
   pub fn set_setting(&self, plugin: &str, key: &str, value: Chosen) {
      self.apply_setting(plugin, key, value);
      self.save_choices();
   }

   fn apply_setting(&self, plugin: &str, key: &str, value: Chosen) {
      self.with(|ctx| {
         let js_value = match &value {
            Chosen::Switch(on) => Ok(Value::new_bool(ctx.clone(), *on)),
            Chosen::Number(number) => Ok(Value::new_float(ctx.clone(), *number)),
            Chosen::Text(text) => {
               rquickjs::String::from_str(ctx.clone(), text).map(rquickjs::String::into_value)
            },
         };
         let callback = ctx.userdata::<Registry<'_>>().and_then(|registry| {
            registry
               .chosen
               .borrow_mut()
               .entry(plugin.to_owned())
               .or_default()
               .insert(key.to_owned(), value);
            redact_secrets(&registry);
            registry
               .defined
               .borrow()
               .get(plugin)
               .and_then(|settings| {
                  settings
                     .definitions
                     .iter()
                     .find(|definition| definition.key == key)
               })
               .and_then(|definition| definition.on_change.clone())
         });
         restart_budget(&ctx);
         resolve_plugin(&ctx, plugin);
         if let Some(callback) = callback
            && let Ok(js_value) = js_value
         {
            restart_budget(&ctx);
            if let Err(err) = callback.call::<_, Value<'_>>((js_value,)).catch(&ctx) {
               log::error!("[{plugin}] {key} onChange: {err}");
            }
            restart_budget(&ctx);
            resolve_plugin(&ctx, plugin);
         }
      });
   }

   pub fn press(&self, plugin: &str, key: &str) {
      self.with(|ctx| {
         let handler = ctx.userdata::<Registry<'_>>().and_then(|registry| {
            registry
               .defined
               .borrow()
               .get(plugin)?
               .definitions
               .iter()
               .find(|definition| definition.key == key)
               .and_then(|definition| {
                  match &definition.kind {
                     Kind::Button(handler) => Some(handler.clone()),
                     _ => None,
                  }
               })
         });
         if let Some(handler) = handler {
            restart_budget(&ctx);
            if let Err(err) = handler.call::<_, ()>(()).catch(&ctx) {
               log::error!("[{plugin}] {key}: {err}");
            }
            restart_budget(&ctx);
            resolve_plugin(&ctx, plugin);
         }
      });
   }
}

/// A plugin's defined settings and the live JS object whose getters read their
/// current values.
pub struct PluginSettings<'js> {
   pub definitions: Vec<Definition<'js>>,
   pub values:      Object<'js>,
}

/// One setting a plugin defined with `phoenix.settings.define`.
pub struct Definition<'js> {
   pub key:         String,
   pub title:       String,
   pub description: Option<String>,
   pub kind:        Kind<'js>,
   /// A text setting edited over several lines.
   pub multiline:   bool,
   on_change:       Option<Function<'js>>,
   visibility:      Visibility<'js>,
   /// Cached result of `visibility`, updated by [`resolve_plugin`].
   visible:         bool,
}

impl Definition<'_> {
   /// Whether this setting should be shown in Settings UI.
   #[must_use]
   pub fn is_shown(&self) -> bool {
      if !self.visible {
         return false;
      }
      match &self.kind {
         Kind::Select {
            options: SelectOptions::Dynamic { resolved, .. },
            ..
         } => resolved.as_ref().is_some_and(|list| !list.is_empty()),
         _ => true,
      }
   }
}

enum Visibility<'js> {
   Always,
   Never,
   Predicate(Function<'js>),
}

/// Choices fixed at `define` time, or computed from current setting values.
pub enum SelectOptions<'js> {
   Static(Vec<Choice>),
   Dynamic {
      source:   Function<'js>,
      /// `None` until the first resolution pass or lazy getter read.
      resolved: Option<Vec<Choice>>,
   },
}

/// A setting's type with its default, or a button's handler.
pub enum Kind<'js> {
   Switch(bool),
   Text(String),
   Secret(String),
   Select {
      options: SelectOptions<'js>,
      default: Option<String>,
   },
   Number(f64),
   Button(Function<'js>),
}

impl Kind<'_> {
   /// The user's choice when it has this kind's type, else the default.
   pub fn current(&self, chosen: Option<&Chosen>) -> Control {
      match (self, chosen) {
         (Self::Switch(_), Some(Chosen::Switch(on))) => Control::Switch(*on),
         (Self::Switch(default), _) => Control::Switch(*default),
         (Self::Text(_), Some(Chosen::Text(text))) => Control::Text(text.clone()),
         (Self::Text(default), _) => Control::Text(default.clone()),
         (Self::Secret(_), Some(Chosen::Text(text))) => Control::Secret(text.clone()),
         (Self::Secret(default), _) => Control::Secret(default.clone()),
         (Self::Select { options, default }, chosen) => {
            match options {
               SelectOptions::Static(choices) => {
                  let fallback = default
                     .as_deref()
                     .or_else(|| choices.first().map(|choice| choice.value.as_str()))
                     .unwrap_or_default();
                  let chosen = match chosen {
                     Some(Chosen::Text(value))
                        if choices.iter().any(|choice| choice.value == *value) =>
                     {
                        value.clone()
                     },
                     _ => fallback.to_owned(),
                  };
                  Control::Select {
                     options: choices.clone(),
                     chosen,
                  }
               },
               SelectOptions::Dynamic { resolved, .. } => {
                  let choices = resolved.as_deref().unwrap_or(&[]);
                  let chosen = match chosen {
                     Some(Chosen::Text(value))
                        if choices.iter().any(|choice| choice.value == *value) =>
                     {
                        value.clone()
                     },
                     _ => {
                        if let Some(def) = default
                           && choices.iter().any(|choice| choice.value == *def)
                        {
                           def.clone()
                        } else if let Some(first) = choices.first() {
                           first.value.clone()
                        } else {
                           default.clone().unwrap_or_default()
                        }
                     },
                  };
                  Control::Select {
                     options: choices.to_vec(),
                     chosen,
                  }
               },
            }
         },
         (Self::Number(_), Some(Chosen::Number(number))) => Control::Number(*number),
         (Self::Number(default), _) => Control::Number(*default),
         (Self::Button(_), _) => Control::Button,
      }
   }

   /// The effective selected value of a select setting, used to detect fixed-point convergence.
   fn selected_value(&self, chosen: Option<&Chosen>) -> Option<String> {
      match self.current(chosen) {
         Control::Select { chosen, .. } => Some(chosen),
         _ => None,
      }
   }
}

#[derive(Clone, Copy)]
enum GuardKind {
   Evaluating,
   Resolving,
}

/// Removes `plugin` from `Registry.evaluating` or `Registry.resolving` on drop,
/// without holding a `RefCell` borrow across JS calls.
pub struct PluginGuard<'a, 'js> {
   ctx:    &'a Ctx<'js>,
   plugin: String,
   kind:   GuardKind,
}

impl<'a, 'js> PluginGuard<'a, 'js> {
   fn enter(ctx: &'a Ctx<'js>, plugin: &str, kind: GuardKind) -> Option<Self> {
      let inserted = ctx.userdata::<Registry<'js>>().is_some_and(|registry| {
         let set = match kind {
            GuardKind::Evaluating => &registry.evaluating,
            GuardKind::Resolving => &registry.resolving,
         };
         set.borrow_mut().insert(plugin.to_owned())
      });
      inserted.then(|| {
         Self {
            ctx,
            plugin: plugin.to_owned(),
            kind,
         }
      })
   }
}

impl Drop for PluginGuard<'_, '_> {
   fn drop(&mut self) {
      if let Some(registry) = self.ctx.userdata::<Registry<'_>>() {
         let set = match self.kind {
            GuardKind::Evaluating => &registry.evaluating,
            GuardKind::Resolving => &registry.resolving,
         };
         set.borrow_mut().remove(&self.plugin);
      }
   }
}

/// Marks `plugin` as currently executing its top-level script so `define()`
/// defers dynamic callbacks until declarations below it have run.
pub fn enter_evaluating<'a, 'js>(ctx: &'a Ctx<'js>, plugin: &str) -> Option<PluginGuard<'a, 'js>> {
   PluginGuard::enter(ctx, plugin, GuardKind::Evaluating)
}

pub fn settings_object<'js>(ctx: &Ctx<'js>, plugin: &str) -> rquickjs::Result<Object<'js>> {
   let settings = Object::new(ctx.clone())?;
   let define_name = plugin.to_owned();
   settings.set(
      "define",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>, schema: Object<'js>| {
         define(&ctx, &define_name, &schema)
      })?,
   )?;
   let refresh_name = plugin.to_owned();
   settings.set(
      "refresh",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>| {
         let skip = ctx.userdata::<Registry<'js>>().is_none_or(|registry| {
            registry.evaluating.borrow().contains(&refresh_name)
               || registry.resolving.borrow().contains(&refresh_name)
         });
         if skip {
            return;
         }
         resolve_plugin(&ctx, &refresh_name);
         crate::refresh_settings();
      })?,
   )?;
   Ok(settings)
}

/// Hands every secret setting's current value to the logger to scrub, including
/// hidden settings whose saved value is still active.
fn redact_secrets(registry: &Registry<'_>) {
   let chosen = registry.chosen.borrow();
   let secrets = registry
      .defined
      .borrow()
      .iter()
      .flat_map(|(plugin, settings)| {
         let picked = chosen.get(plugin);
         settings.definitions.iter().filter_map(move |definition| {
            match definition
               .kind
               .current(picked.and_then(|values| values.get(&definition.key)))
            {
               Control::Secret(value) => Some(value),
               _ => None,
            }
         })
      })
      .collect::<Vec<String>>();
   crate::console::redact(secrets);
}

fn budget_spent(ctx: &Ctx<'_>) -> bool {
   ctx.userdata::<Registry<'_>>()
      .is_some_and(|registry| registry.budget.spent())
}

fn is_truthy(value: &Value<'_>) -> bool {
   if value.is_undefined() || value.is_null() {
      return false;
   }
   if let Some(flag) = value.as_bool() {
      return flag;
   }
   if let Some(int) = value.as_int() {
      return int != 0;
   }
   if let Some(float) = value.as_float() {
      return float != 0.0 && !float.is_nan();
   }
   if let Some(text) = value.as_string() {
      return text.to_string().is_ok_and(|value| !value.is_empty());
   }
   if let Some(big_int) = value.clone().into_big_int() {
      return !big_int.to_i64().is_ok_and(|int| int == 0);
   }
   true
}

/// Parses a JS array of `{ value, label? }` objects, keeping the first entry
/// for each distinct `value`.
fn parse_choices<'js>(
   ctx: &Ctx<'js>,
   key: &str,
   value: Value<'js>,
) -> rquickjs::Result<Vec<Choice>> {
   let Some(array) = value.into_array() else {
      return Err(Exception::throw_type(
         ctx,
         &format!("setting {key}: select options must be an array"),
      ));
   };
   let mut seen = HashSet::new();
   // Cap pre-allocation so a sparse JS array like `new Array(1e9)` cannot
   // trigger a multi-gigabyte Rust allocation before element decoding rejects it.
   let mut choices = Vec::with_capacity(array.len().min(64));
   for entry in array.iter::<Object<'js>>() {
      let option = entry?;
      let value: String = option.get("value")?;
      if !seen.insert(value.clone()) {
         continue;
      }
      let label = option.get::<_, Option<String>>("label")?;
      choices.push(Choice {
         label: label.unwrap_or_else(|| value.clone()),
         value,
      });
   }
   Ok(choices)
}

fn parse_visibility<'js>(
   ctx: &Ctx<'js>,
   key: &str,
   spec: &Object<'js>,
) -> rquickjs::Result<(Visibility<'js>, bool)> {
   let raw: Value<'js> = spec.get("visible")?;
   if raw.is_undefined() {
      return Ok((Visibility::Always, true));
   }
   if let Some(flag) = raw.as_bool() {
      let visibility = if flag {
         Visibility::Always
      } else {
         Visibility::Never
      };
      return Ok((visibility, flag));
   }
   if let Some(func) = raw.into_function() {
      return Ok((Visibility::Predicate(func), true));
   }
   Err(Exception::throw_type(
      ctx,
      &format!("setting {key}: visible must be a boolean or a function"),
   ))
}

/// The kind of setting `key` that `spec` describes.
fn setting_kind<'js>(ctx: &Ctx<'js>, key: &str, spec: &Object<'js>) -> rquickjs::Result<Kind<'js>> {
   Ok(match spec.get::<_, String>("type")?.as_str() {
      "switch" => Kind::Switch(spec.get::<_, Option<bool>>("default")?.unwrap_or_default()),
      "text" => {
         Kind::Text(
            spec
               .get::<_, Option<String>>("default")?
               .unwrap_or_default(),
         )
      },
      "secret" => {
         Kind::Secret(
            spec
               .get::<_, Option<String>>("default")?
               .unwrap_or_default(),
         )
      },
      "select" => {
         let raw: Value<'js> = spec.get("options")?;
         let default = spec.get::<_, Option<String>>("default")?;
         if let Some(source) = raw.clone().into_function() {
            Kind::Select {
               options: SelectOptions::Dynamic {
                  source,
                  resolved: None,
               },
               default,
            }
         } else {
            let options = parse_choices(ctx, key, raw)?;
            let Some(first) = options.first() else {
               return Err(Exception::throw_type(
                  ctx,
                  &format!("setting {key}: a select needs at least one option"),
               ));
            };
            let default = Some(default.unwrap_or_else(|| first.value.clone()));
            Kind::Select {
               options: SelectOptions::Static(options),
               default,
            }
         }
      },
      "number" => Kind::Number(spec.get::<_, Option<f64>>("default")?.unwrap_or_default()),
      "button" => Kind::Button(spec.get("onPress")?),
      other => {
         return Err(Exception::throw_type(
            ctx,
            &format!(
               "setting {key}: unknown type {other:?}, expected switch, text, secret, select, \
                number or button"
            ),
         ));
      },
   })
}

fn set_visible(ctx: &Ctx<'_>, plugin: &str, key: &str, visible: bool) {
   if let Some(registry) = ctx.userdata::<Registry<'_>>()
      && let Some(definition) = registry
         .defined
         .borrow_mut()
         .get_mut(plugin)
         .and_then(|settings| settings.definitions.iter_mut().find(|d| d.key == key))
   {
      definition.visible = visible;
   }
}

/// Lazily resolves a single dynamic select on its first getter read during
/// top-level evaluation, before the end-of-eval pass has run.
fn resolve_single_select<'js>(ctx: &Ctx<'js>, plugin: &str, key: &str) {
   let Some(_guard) = PluginGuard::enter(ctx, plugin, GuardKind::Resolving) else {
      return;
   };
   let pending = ctx.userdata::<Registry<'js>>().and_then(|registry| {
      let defined = registry.defined.borrow();
      let settings = defined.get(plugin)?;
      let definition = settings.definitions.iter().find(|d| d.key == key)?;
      match &definition.kind {
         Kind::Select {
            options: SelectOptions::Dynamic {
               source,
               resolved: None,
            },
            ..
         } => Some((source.clone(), settings.values.clone())),
         _ => None,
      }
   });
   let Some((source, values)) = pending else {
      return;
   };
   let outcome = source
      .call::<_, Value<'js>>((values,))
      .and_then(|raw| parse_choices(ctx, key, raw))
      .catch(ctx);
   match outcome {
      Ok(choices) => {
         if let Some(registry) = ctx.userdata::<Registry<'js>>()
            && let Some(definition) = registry
               .defined
               .borrow_mut()
               .get_mut(plugin)
               .and_then(|settings| settings.definitions.iter_mut().find(|d| d.key == key))
            && let Kind::Select {
               options: SelectOptions::Dynamic { resolved, .. },
               ..
            } = &mut definition.kind
         {
            *resolved = Some(choices);
         }
      },
      Err(err) => {
         log::error!("[{plugin}] {key} options: {err}");
      },
   }
}

/// Evaluates `plugin`'s dynamic select options (in a fixed-point loop) and then
/// its visibility predicates, caching both on each [`Definition`].
///
/// Runs under the caller's existing `QuickJS` budget so JS-triggered `refresh()`
/// and late `define()` cannot reset the interrupt deadline; Rust entrypoints
/// call [`restart_budget`] before invoking this.
pub fn resolve_plugin<'js>(ctx: &Ctx<'js>, plugin: &str) {
   let Some(_guard) = PluginGuard::enter(ctx, plugin, GuardKind::Resolving) else {
      return;
   };
   let Some(values) = ctx.userdata::<Registry<'js>>().and_then(|registry| {
      registry
         .defined
         .borrow()
         .get(plugin)
         .map(|settings| settings.values.clone())
   }) else {
      return;
   };

   if !resolve_dynamic_selects(ctx, plugin, &values) {
      return;
   }
   resolve_visibilities(ctx, plugin, &values);
}

/// Runs Pass 1 (dynamic select options) up to [`MAX_RESOLVE_PASSES`] times
/// until no select's effective value changes. Returns `false` if the budget
/// ran out so the caller skips Pass 2.
fn resolve_dynamic_selects<'js>(ctx: &Ctx<'js>, plugin: &str, values: &Object<'js>) -> bool {
   for _ in 0..MAX_RESOLVE_PASSES {
      let dynamic = ctx
         .userdata::<Registry<'js>>()
         .and_then(|registry| {
            registry.defined.borrow().get(plugin).map(|settings| {
               settings
                  .definitions
                  .iter()
                  .filter_map(|definition| {
                     match &definition.kind {
                        Kind::Select {
                           options: SelectOptions::Dynamic { source, .. },
                           ..
                        } => Some((definition.key.clone(), source.clone())),
                        _ => None,
                     }
                  })
                  .collect::<Vec<_>>()
            })
         })
         .unwrap_or_default();
      if dynamic.is_empty() {
         break;
      }

      let mut changed = false;
      for (key, source) in dynamic {
         let outcome = source
            .call::<_, Value<'js>>((values.clone(),))
            .and_then(|raw| parse_choices(ctx, &key, raw))
            .catch(ctx);
         match outcome {
            Ok(choices) => {
               if let Some(registry) = ctx.userdata::<Registry<'js>>() {
                  let chosen = registry.chosen.borrow();
                  let picked = chosen.get(plugin).and_then(|values| values.get(&key));
                  let mut defined = registry.defined.borrow_mut();
                  if let Some(definition) = defined
                     .get_mut(plugin)
                     .and_then(|settings| settings.definitions.iter_mut().find(|d| d.key == key))
                  {
                     let before = definition.kind.selected_value(picked);
                     if let Kind::Select {
                        options: SelectOptions::Dynamic { resolved, .. },
                        ..
                     } = &mut definition.kind
                     {
                        *resolved = Some(choices);
                     }
                     let after = definition.kind.selected_value(picked);
                     if before != after {
                        changed = true;
                     }
                  }
               }
            },
            Err(err) => {
               log::error!("[{plugin}] {key} options: {err}");
               if budget_spent(ctx) {
                  return false;
               }
            },
         }
      }
      if !changed {
         break;
      }
   }
   true
}

/// Runs Pass 2 (visibility predicates) against the settled `values` object.
fn resolve_visibilities<'js>(ctx: &Ctx<'js>, plugin: &str, values: &Object<'js>) {
   let predicates = ctx
      .userdata::<Registry<'js>>()
      .and_then(|registry| {
         registry.defined.borrow().get(plugin).map(|settings| {
            settings
               .definitions
               .iter()
               .filter_map(|definition| {
                  match &definition.visibility {
                     Visibility::Predicate(func) => Some((definition.key.clone(), func.clone())),
                     Visibility::Always | Visibility::Never => None,
                  }
               })
               .collect::<Vec<_>>()
         })
      })
      .unwrap_or_default();

   for (key, predicate) in predicates {
      let visible = match predicate.call::<_, Value<'js>>((values.clone(),)).catch(ctx) {
         Ok(value) => is_truthy(&value),
         Err(err) => {
            log::error!("[{plugin}] {key} visible: {err}");
            if budget_spent(ctx) {
               set_visible(ctx, plugin, &key, true);
               return;
            }
            true
         },
      };
      set_visible(ctx, plugin, &key, visible);
   }
}

/// Records `schema` as the plugin's settings and returns an object with one
/// getter per non-button key that reads the user's current choice.
fn define<'js>(
   ctx: &Ctx<'js>,
   plugin: &str,
   schema: &Object<'js>,
) -> rquickjs::Result<Object<'js>> {
   let in_resolving = ctx
      .userdata::<Registry<'js>>()
      .ok_or_else(|| Exception::throw_internal(ctx, "no registry"))?
      .resolving
      .borrow()
      .contains(plugin);
   if in_resolving {
      return Err(Exception::throw_message(
         ctx,
         "phoenix.settings.define cannot be called inside a settings callback",
      ));
   }

   let mut definitions = Vec::new();
   for entry in schema.props::<String, Object<'js>>() {
      let (key, spec) = entry?;
      let kind = setting_kind(ctx, &key, &spec)?;
      let (visibility, visible) = parse_visibility(ctx, &key, &spec)?;
      definitions.push(Definition {
         title: spec.get("title")?,
         description: spec.get("description")?,
         multiline: spec.get::<_, Option<bool>>("multiline")?.unwrap_or_default(),
         on_change: spec.get("onChange")?,
         kind,
         key,
         visibility,
         visible,
      });
   }

   let values = Object::new(ctx.clone())?;
   for definition in &definitions {
      if matches!(definition.kind, Kind::Button(_)) {
         continue;
      }
      let (plugin, key) = (plugin.to_owned(), definition.key.clone());
      let read = move |ctx: Ctx<'js>| -> rquickjs::Result<Value<'js>> {
         let needs_lazy_resolve = ctx.userdata::<Registry<'js>>().is_some_and(|registry| {
            registry.evaluating.borrow().contains(&plugin)
               && registry
                  .defined
                  .borrow()
                  .get(&plugin)
                  .and_then(|settings| settings.definitions.iter().find(|d| d.key == key))
                  .is_some_and(|definition| {
                     matches!(
                        definition.kind,
                        Kind::Select {
                           options: SelectOptions::Dynamic { resolved: None, .. },
                           ..
                        }
                     )
                  })
         });
         if needs_lazy_resolve {
            resolve_single_select(&ctx, &plugin, &key);
         }

         let registry = ctx
            .userdata::<Registry<'js>>()
            .ok_or_else(|| Exception::throw_internal(&ctx, "no registry"))?;
         let defined = registry.defined.borrow();
         let chosen = registry.chosen.borrow();
         let Some(definition) = defined
            .get(&plugin)
            .and_then(|settings| settings.definitions.iter().find(|d| d.key == key))
         else {
            return Ok(Value::new_undefined(ctx.clone()));
         };
         let picked = chosen.get(&plugin).and_then(|values| values.get(&key));
         match definition.kind.current(picked) {
            Control::Switch(on) => on.into_js(&ctx),
            Control::Text(text) | Control::Secret(text) => text.into_js(&ctx),
            Control::Select { chosen, .. } => chosen.into_js(&ctx),
            Control::Number(number) => number.into_js(&ctx),
            Control::Button => Ok(Value::new_undefined(ctx.clone())),
         }
      };
      values.prop(
         definition.key.as_str(),
         Accessor::new_get(read).enumerable(),
      )?;
   }

   let evaluating = {
      let registry = ctx
         .userdata::<Registry<'js>>()
         .ok_or_else(|| Exception::throw_internal(ctx, "no registry"))?;
      registry.defined.borrow_mut().insert(
         plugin.to_owned(),
         PluginSettings {
            definitions,
            values: values.clone(),
         },
      );
      redact_secrets(&registry);
      registry.evaluating.borrow().contains(plugin)
   };

   if !evaluating {
      resolve_plugin(ctx, plugin);
      crate::refresh_settings();
   }
   Ok(values)
}

#[cfg(test)]
mod tests {
   use std::time::{
      Duration,
      Instant,
   };

   use phoenix_sigs::registry::GatewayRegistry;

   use super::*;
   use crate::{
      PluginState,
      Setting,
      js::{
         Loaded,
         Origin,
         compile,
      },
   };

   fn eval_settings_plugin_returning_fn(
      engine: &mut Engine,
      name: &str,
      source: &str,
   ) -> Result<rquickjs::Persistent<Function<'static>>, String> {
      let mut loaded = Loaded::new(source.to_owned(), Origin::Bundled);
      let outcome = engine.with(|ctx| {
         let evaluating = enter_evaluating(&ctx, name);
         let run = || -> rquickjs::Result<rquickjs::Persistent<Function<'static>>> {
            let phoenix = Object::new(ctx.clone())?;
            phoenix.set("settings", settings_object(&ctx, name)?)?;
            let func: Function<'_> = compile(&ctx, source)?.call((phoenix,))?;
            Ok(rquickjs::Persistent::save(&ctx, func))
         };
         let outcome = run().catch(&ctx).map_err(|err| err.to_string());
         drop(evaluating);
         if outcome.is_ok() {
            restart_budget(&ctx);
            resolve_plugin(&ctx, name);
         }
         outcome
      });
      if outcome.is_ok() {
         loaded.state = PluginState::On;
      }
      engine.plugins.insert(name.to_owned(), loaded);
      outcome
   }

   fn eval_settings_plugin(engine: &mut Engine, name: &str, source: &str) -> Result<(), String> {
      let mut loaded = Loaded::new(source.to_owned(), Origin::Bundled);
      let outcome = engine.with(|ctx| {
         let evaluating = enter_evaluating(&ctx, name);
         let run = || -> rquickjs::Result<()> {
            let phoenix = Object::new(ctx.clone())?;
            phoenix.set("settings", settings_object(&ctx, name)?)?;
            compile(&ctx, source)?.call((phoenix,))
         };
         let outcome = run().catch(&ctx).map_err(|err| err.to_string());
         drop(evaluating);
         if outcome.is_ok() {
            restart_budget(&ctx);
            resolve_plugin(&ctx, name);
         }
         outcome
      });
      if outcome.is_ok() {
         loaded.state = PluginState::On;
      }
      engine.plugins.insert(name.to_owned(), loaded);
      outcome
   }

   fn shown(engine: &Engine, name: &str) -> Vec<Setting> {
      engine
         .plugins()
         .into_iter()
         .find(|plugin| plugin.name == name)
         .map_or_default(|plugin| plugin.settings)
   }

   fn keys(engine: &Engine, name: &str) -> Vec<String> {
      shown(engine, name).into_iter().map(|s| s.key).collect()
   }

   fn read_js_value(engine: &Engine, plugin: &str, key: &str) -> String {
      engine.with(|ctx| {
         let registry = ctx.userdata::<Registry<'_>>().expect("registry");
         let values = registry
            .defined
            .borrow()
            .get(plugin)
            .expect("plugin defined")
            .values
            .clone();
         let val: Value<'_> = values.get(key).expect("getter");
         val.as_string()
            .map(|s| s.to_string().expect("utf8"))
            .or_else(|| val.as_bool().map(|b| b.to_string()))
            .or_else(|| val.as_int().map(|n| n.to_string()))
            .or_else(|| val.as_float().map(|n| n.to_string()))
            .unwrap_or_else(|| "other".to_owned())
      })
   }

   #[test]
   fn static_and_predicate_visibility_and_truthiness() {
      let mut engine = Engine::new(&GatewayRegistry::default()).expect("engine");

      // Invalid visible type throws TypeError.
      assert!(
         eval_settings_plugin(
            &mut engine,
            "bad",
            r#"phoenix.settings.define({ a: { type: "switch", title: "A", visible: 123 } });"#,
         )
         .is_err()
      );

      eval_settings_plugin(
         &mut engine,
         "vis",
         r#"
            phoenix.settings.define({
               always: { type: "switch", title: "Always", visible: true },
               never: { type: "switch", title: "Never", visible: false },
               toggle: { type: "switch", title: "Toggle", default: false },
               count: { type: "number", title: "Count", default: 0 },
               text: { type: "text", title: "Text", default: "" },
               fromSwitch: { type: "text", title: "FromSwitch", visible: (s) => s.toggle },
               fromNumber: { type: "text", title: "FromNumber", visible: (s) => s.count },
               fromText: { type: "text", title: "FromText", visible: (s) => s.text },
               fromNaN: { type: "text", title: "FromNaN", visible: () => NaN },
               fromNull: { type: "text", title: "FromNull", visible: () => null },
            });
         "#,
      )
      .expect("eval");

      assert_eq!(
         keys(&engine, "vis"),
         vec!["always", "toggle", "count", "text"]
      );

      engine.apply_setting("vis", "toggle", Chosen::Switch(true));
      engine.apply_setting("vis", "count", Chosen::Number(3.0));
      engine.apply_setting("vis", "text", Chosen::Text("hi".to_owned()));
      assert_eq!(
         keys(&engine, "vis"),
         vec![
            "always",
            "toggle",
            "count",
            "text",
            "fromSwitch",
            "fromNumber",
            "fromText"
         ]
      );

      engine.apply_setting("vis", "count", Chosen::Number(0.0));
      engine.apply_setting("vis", "text", Chosen::Text(String::new()));
      assert_eq!(
         keys(&engine, "vis"),
         vec!["always", "toggle", "count", "text", "fromSwitch"]
      );
   }

   #[test]
   fn hidden_preserves_saved_value_and_redacts_secrets() {
      let mut engine = Engine::new(&GatewayRegistry::default()).expect("engine");
      eval_settings_plugin(
         &mut engine,
         "tr",
         r#"
            phoenix.settings.define({
               service: {
                  type: "select",
                  title: "Service",
                  options: [{ value: "google" }, { value: "kagi" }],
               },
               token: {
                  type: "secret",
                  title: "Token",
                  visible: (s) => s.service === "kagi",
               },
            });
         "#,
      )
      .expect("eval");

      assert_eq!(keys(&engine, "tr"), vec!["service"]);

      engine.apply_setting("tr", "service", Chosen::Text("kagi".to_owned()));
      assert_eq!(keys(&engine, "tr"), vec!["service", "token"]);

      engine.apply_setting("tr", "token", Chosen::Text("super-secret-xyz".to_owned()));
      assert_eq!(read_js_value(&engine, "tr", "token"), "super-secret-xyz");

      // Hide token again: UI omits it, saved value stays readable in JS and redacted in logs.
      engine.apply_setting("tr", "service", Chosen::Text("google".to_owned()));
      assert_eq!(keys(&engine, "tr"), vec!["service"]);
      assert_eq!(read_js_value(&engine, "tr", "token"), "super-secret-xyz");
      assert_eq!(
         crate::console::scrub("leaked super-secret-xyz here".to_owned()),
         "leaked [redacted] here"
      );

      // Re-showing restores the saved value to the UI.
      engine.apply_setting("tr", "service", Chosen::Text("kagi".to_owned()));
      let settings = shown(&engine, "tr");
      assert_eq!(settings.len(), 2);
      assert_eq!(
         settings[1].control,
         Control::Secret("super-secret-xyz".to_owned())
      );
   }

   #[test]
   fn dynamic_select_fallback_dedupe_and_empty_hiding() {
      let mut engine = Engine::new(&GatewayRegistry::default()).expect("engine");

      // Empty static select still throws TypeError.
      assert!(
         eval_settings_plugin(
            &mut engine,
            "empty_static",
            r#"phoenix.settings.define({ a: { type: "select", title: "A", options: [] } });"#,
         )
         .is_err()
      );

      eval_settings_plugin(
         &mut engine,
         "dyn",
         r#"
            phoenix.settings.define({
               stat: {
                  type: "select",
                  title: "Static",
                  options: [
                     { value: "dup", label: "First" },
                     { value: "dup", label: "Second" },
                     { value: "other" },
                  ],
               },
               mode: {
                  type: "select",
                  title: "Mode",
                  options: [{ value: "a" }, { value: "b" }, { value: "empty" }],
               },
               choice: {
                  type: "select",
                  title: "Choice",
                  options: (s) => {
                     if (s.mode === "a") {
                        return [
                           { value: "a1", label: "A1 first" },
                           { value: "a1", label: "A1 dup" },
                           { value: "a2", label: "A2" },
                        ];
                     }
                     if (s.mode === "b") return [{ value: "b1" }, { value: "b2" }];
                     return [];
                  },
               },
            });
         "#,
      )
      .expect("eval");

      // Static and dynamic duplicate values keep the first entry.
      let s = shown(&engine, "dyn");
      assert_eq!(
         s[0].control,
         Control::Select {
            options: vec![
               Choice {
                  value: "dup".to_owned(),
                  label: "First".to_owned(),
               },
               Choice {
                  value: "other".to_owned(),
                  label: "other".to_owned(),
               },
            ],
            chosen:  "dup".to_owned(),
         }
      );
      assert_eq!(
         s[2].control,
         Control::Select {
            options: vec![
               Choice {
                  value: "a1".to_owned(),
                  label: "A1 first".to_owned(),
               },
               Choice {
                  value: "a2".to_owned(),
                  label: "A2".to_owned(),
               },
            ],
            chosen:  "a1".to_owned(),
         }
      );

      // Pick a2, switch mode to b -> falls back to b1 without erasing saved a2.
      engine.apply_setting("dyn", "choice", Chosen::Text("a2".to_owned()));
      assert_eq!(read_js_value(&engine, "dyn", "choice"), "a2");

      engine.apply_setting("dyn", "mode", Chosen::Text("b".to_owned()));
      assert_eq!(read_js_value(&engine, "dyn", "choice"), "b1");

      // Switch to empty -> select is hidden in UI and reads "" in JS.
      engine.apply_setting("dyn", "mode", Chosen::Text("empty".to_owned()));
      assert_eq!(keys(&engine, "dyn"), vec!["stat", "mode"]);
      assert_eq!(read_js_value(&engine, "dyn", "choice"), "");

      // Switch back to a -> saved a2 is restored.
      engine.apply_setting("dyn", "mode", Chosen::Text("a".to_owned()));
      assert_eq!(keys(&engine, "dyn"), vec!["stat", "mode", "choice"]);
      assert_eq!(read_js_value(&engine, "dyn", "choice"), "a2");
   }

   #[test]
   fn cascading_and_reversed_order_selects() {
      let mut engine = Engine::new(&GatewayRegistry::default()).expect("engine");
      eval_settings_plugin(
         &mut engine,
         "cascade",
         r#"
            phoenix.settings.define({
               subitem: {
                  type: "select",
                  title: "Sub",
                  options: (s) => s.item === "b1"
                     ? [{ value: "b1x" }]
                     : [{ value: "a1x" }],
               },
               item: {
                  type: "select",
                  title: "Item",
                  options: (s) => s.cat === "b"
                     ? [{ value: "b1" }]
                     : [{ value: "a1" }],
               },
               cat: {
                  type: "select",
                  title: "Cat",
                  options: [{ value: "a" }, { value: "b" }],
               },
            });
         "#,
      )
      .expect("eval");

      assert_eq!(read_js_value(&engine, "cascade", "item"), "a1");
      assert_eq!(read_js_value(&engine, "cascade", "subitem"), "a1x");

      engine.apply_setting("cascade", "cat", Chosen::Text("b".to_owned()));
      assert_eq!(read_js_value(&engine, "cascade", "item"), "b1");
      assert_eq!(read_js_value(&engine, "cascade", "subitem"), "b1x");
   }

   #[test]
   fn startup_tdz_lazy_read_and_late_define() {
      let mut engine = Engine::new(&GatewayRegistry::default()).expect("engine");
      eval_settings_plugin(
         &mut engine,
         "tdz",
         r#"
            const settings = phoenix.settings.define({
               theme: {
                  type: "select",
                  title: "Theme",
                  options: () => listThemes(),
               },
               extra: {
                  type: "text",
                  title: "Extra",
                  visible: () => showExtra(),
               },
            });
            const listThemes = () => [{ value: "dracula" }, { value: "nord" }];
            const showExtra = () => false;
            const initial = settings.theme;
            if (initial !== "dracula") throw new Error("expected lazy read dracula, got " + initial);
         "#,
      )
      .expect("tdz eval");

      assert_eq!(keys(&engine, "tdz"), vec!["theme"]);
      assert_eq!(read_js_value(&engine, "tdz", "theme"), "dracula");

      // Late define outside evaluating resolves immediately.
      let late_define = eval_settings_plugin_returning_fn(
         &mut engine,
         "late",
         r#"
            return () => {
               phoenix.settings.define({
                  a: { type: "switch", title: "A", default: false },
                  b: { type: "text", title: "B", visible: (s) => s.a },
               });
            };
         "#,
      )
      .expect("late setup");
      engine.with(|ctx| {
         late_define
            .restore(&ctx)
            .expect("restore")
            .call::<_, ()>(())
            .expect("call");
      });
      assert_eq!(keys(&engine, "late"), vec!["a"]);
   }

   #[test]
   fn refresh_press_and_onchange_resolve_external_state() {
      let mut engine = Engine::new(&GatewayRegistry::default()).expect("engine");
      let flip_external = eval_settings_plugin_returning_fn(
         &mut engine,
         "ext",
         r#"
            let loggedIn = false;
            let extra = false;
            phoenix.settings.define({
               login: {
                  type: "button",
                  title: "Login",
                  visible: () => !loggedIn,
                  onPress: () => { loggedIn = true; },
               },
               logout: {
                  type: "button",
                  title: "Logout",
                  visible: () => loggedIn,
                  onPress: () => { loggedIn = false; },
               },
               trigger: {
                  type: "switch",
                  title: "Trigger",
                  onChange: (v) => { extra = v; },
               },
               fromOnChange: {
                  type: "text",
                  title: "FromOnChange",
                  visible: () => extra,
               },
            });
            return () => {
               loggedIn = !loggedIn;
               phoenix.settings.refresh();
            };
         "#,
      )
      .expect("eval");

      assert_eq!(keys(&engine, "ext"), vec!["login", "trigger"]);

      engine.press("ext", "login");
      assert_eq!(keys(&engine, "ext"), vec!["logout", "trigger"]);

      engine.apply_setting("ext", "trigger", Chosen::Switch(true));
      assert_eq!(
         keys(&engine, "ext"),
         vec!["logout", "trigger", "fromOnChange"]
      );

      engine.with(|ctx| {
         flip_external
            .restore(&ctx)
            .expect("restore")
            .call::<_, ()>(())
            .expect("call");
      });
      assert_eq!(
         keys(&engine, "ext"),
         vec!["login", "trigger", "fromOnChange"]
      );
   }

   #[test]
   fn resilience_errors_reentrancy_and_budget() {
      let mut engine = Engine::new(&GatewayRegistry::default()).expect("engine");
      let trigger_break = eval_settings_plugin_returning_fn(
         &mut engine,
         "res",
         r#"
            let breakOptions = false;
            let reentrantDefineFailed = false;
            phoenix.settings.define({
               flag: { type: "switch", title: "Flag", default: false },
               brokenVis: {
                  type: "text",
                  title: "BrokenVis",
                  visible: () => { throw new Error("boom"); },
               },
               sel: {
                  type: "select",
                  title: "Sel",
                  options: () => {
                     phoenix.settings.refresh();
                     try {
                        phoenix.settings.define({ x: { type: "switch", title: "X" } });
                     } catch {
                        reentrantDefineFailed = true;
                     }
                     if (breakOptions) return "not an array";
                     return [{ value: "ok" }];
                  },
               },
            });
            return () => {
               breakOptions = true;
               phoenix.settings.refresh();
               return reentrantDefineFailed;
            };
         "#,
      )
      .expect("eval");

      // Thrown visible fails open; re-entrant refresh/define did not corrupt definitions.
      assert_eq!(keys(&engine, "res"), vec!["flag", "brokenVis", "sel"]);

      // Malformed dynamic options preserves prior resolved choices.
      let rejected = engine.with(|ctx| {
         trigger_break
            .restore(&ctx)
            .expect("restore")
            .call::<_, bool>(())
            .expect("call")
      });
      assert!(rejected);
      assert_eq!(keys(&engine, "res"), vec!["flag", "brokenVis", "sel"]);
      assert_eq!(read_js_value(&engine, "res", "sel"), "ok");

      // Spinning callback hits the 1s budget once and aborts the rest of the pass early.
      let started = Instant::now();
      let ran_second_fn = eval_settings_plugin_returning_fn(
         &mut engine,
         "spin",
         r#"
            let ranSecond = false;
            phoenix.settings.define({
               first: {
                  type: "text",
                  title: "First",
                  visible: () => { while (true) {} },
               },
               second: {
                  type: "text",
                  title: "Second",
                  visible: () => { ranSecond = true; return false; },
               },
            });
            return () => ranSecond;
         "#,
      )
      .expect("spin eval");
      assert!(started.elapsed() < Duration::from_secs(3));
      let ran_second = engine.with(|ctx| {
         ran_second_fn
            .restore(&ctx)
            .expect("restore")
            .call::<_, bool>(())
            .expect("call")
      });
      assert!(!ran_second);
   }

   #[test]
   fn refresh_budget_sparse_arrays_and_no_post_eval_reinvoke() {
      let mut engine = Engine::new(&GatewayRegistry::default()).expect("engine");

      // Calling refresh() in a loop cannot bypass the 1s execution budget.
      let loop_started = Instant::now();
      let spin_refresh = eval_settings_plugin_returning_fn(
         &mut engine,
         "spin_refresh",
         r#"
            phoenix.settings.define({
               a: { type: "switch", title: "A" },
            });
            return () => {
               while (true) {
                  phoenix.settings.refresh();
               }
            };
         "#,
      )
      .expect("spin_refresh setup");
      let interrupted = engine.with(|ctx| {
         spin_refresh
            .restore(&ctx)
            .expect("restore")
            .call::<_, ()>(())
            .catch(&ctx)
            .is_err()
      });
      assert!(interrupted);
      assert!(loop_started.elapsed() < Duration::from_secs(3));

      // Sparse huge arrays fail cleanly without Rust OOM pre-allocation.
      assert!(
         eval_settings_plugin(
            &mut engine,
            "sparse_static",
            r#"phoenix.settings.define({ a: { type: "select", title: "A", options: new Array(1e9) } });"#,
         )
         .is_err()
      );
      eval_settings_plugin(
         &mut engine,
         "sparse_dyn",
         r#"phoenix.settings.define({ a: { type: "select", title: "A", options: () => new Array(1e9) } });"#,
      )
      .expect("sparse_dyn eval");
      assert_eq!(keys(&engine, "sparse_dyn"), Vec::<String>::new());

      // Failed dynamic select is not lazily re-invoked on post-eval getter reads.
      let call_count = eval_settings_plugin_returning_fn(
         &mut engine,
         "no_reinvoke",
         r#"
            let calls = 0;
            phoenix.settings.define({
               sel: {
                  type: "select",
                  title: "Sel",
                  options: () => {
                     calls += 1;
                     throw new Error("always fails");
                  },
               },
            });
            return () => calls;
         "#,
      )
      .expect("no_reinvoke eval");
      assert_eq!(read_js_value(&engine, "no_reinvoke", "sel"), "");
      assert_eq!(read_js_value(&engine, "no_reinvoke", "sel"), "");
      let calls = engine.with(|ctx| {
         call_count
            .restore(&ctx)
            .expect("restore")
            .call::<_, i32>(())
            .expect("call")
      });
      assert_eq!(calls, 1);
   }
}
