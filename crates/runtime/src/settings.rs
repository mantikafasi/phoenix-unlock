//! A "Phoenix Unlocker" row in Discord's Settings that opens phoenix's own pages.
//!
//! Phoenix builds its UI as a stream of UI-op batches from Rust to Java. This
//! module rewrites those batches to splice in nodes built from Phoenix's own
//! layouts, and answers view events on them. Injected nodes use ids from
//! `0x5000_0000`, far above the counter Rust allocates from.

use std::{
   collections::{
      HashMap,
      HashSet,
   },
   sync::{
      LazyLock,
      Mutex,
      MutexGuard,
      OnceLock,
      PoisonError,
      atomic::{
         AtomicBool,
         Ordering,
      },
      mpsc::{
         self,
         Sender,
      },
   },
   thread,
};

use memchr::memmem;
use phoenix_ui::{
   ChildEdit,
   Node,
   Op,
   Prop,
   Props,
   ViewKind,
   decode,
   encode,
   motion::{
      self,
      Spring,
      Worklet,
   },
   set_prop,
   templates,
};
use phoenix_wire::{
   DispatchEvent,
   Value,
   decode_dispatch,
   without_frames,
};

use crate::{
   Chosen,
   Control,
   PluginInfo,
   highlight::{
      self,
      Kind,
   },
   bridge::{
      self,
      Action,
      Sink,
   },
};

const FIRST_ID: i32 = 0x5000_0000;
/// The host drops motion slots at or above 65536, and Rust counts its own up
/// from 0, so phoenix takes them downward from the top.
const SLOT_END: i32 = 1 << 16;
const LOGOUT_ROW: &str = "settings.row.logout";
/// The Settings row and the page it opens.
const TITLE: &str = "Phoenix Unlocker";
const ICON_PLUGIN: &str = "res://ic_mana_puzzle_piece_plus";
const ICON_CHECK: &str = "res://ic_mana_checkmark_large";
const ICON_RELOAD: &str = "res://ic_mana_puzzle_piece_retry";
const ICON_UNINSTALL: &str = "res://ic_mana_puzzle_piece_minus";
const ICON_CONSOLE: &str = "res://ic_mana_angle_brackets";
const ICON_FILE: &str = "res://ic_mana_image_file_up";
/// Text colors, ARGB, for Discord's dark theme.
const COLOR_TEXT: i64 = 0xFFF6_F6F7;
const COLOR_MUTED: i64 = 0xFF94_9BA4;
const COLOR_WARN: i64 = 0xFFF0_B232;
const COLOR_ERROR: i64 = 0xFFF2_3F42;
const COLOR_ACCENT: i64 = 0xFF79_C0FF;
const COLOR_TAG: i64 = 0xFFD2_A8FF;
const COLOR_STRING: i64 = 0xFF7E_E787;
const COLOR_KEYWORD: i64 = 0xFFFF_7B72;
const COLOR_NUMBER: i64 = 0xFF79_C0FF;
const COLOR_COMMENT: i64 = 0xFF8B_949E;
/// The native hook libraries linked in: inline hooks (as `phoenix_hook::Inline`
/// picks them) and bytehook for PLT hooks.
const HOOK_LIBRARY: &str = if cfg!(any(target_arch = "aarch64", target_arch = "arm")) {
   "ShadowHook + bytehook"
} else {
   "Dobby + bytehook"
};
/// Lines the console page shows, newest first.
const LOG_LINES: usize = 150;
/// Plugins larger than this are not shown in the code editor.
const MAX_EDIT: usize = 100 * 1024;
/// Ids Phoenix's captured page layout uses for the parts phoenix fills in.
const PAGE_HEADER_SPACER: u32 = 0x4E1;
const PAGE_CONTENT: u32 = 0x4E4;
const PAGE_HEADER: u32 = 0x562;
const PAGE_BACK: u32 = 0x565;
const PAGE_TITLE: u32 = 0x56A;

const RADII: [Prop; 5] = [
   Prop::RADIUS,
   Prop::RADIUS_TOP_LEFT,
   Prop::RADIUS_TOP_RIGHT,
   Prop::RADIUS_BOTTOM_LEFT,
   Prop::RADIUS_BOTTOM_RIGHT,
];

/// Plugins are named by their index in [`crate::plugins`] as of the last
/// render, which every install follows with a fresh one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Press {
   OpenMain,
   OpenConsole,
   OpenInstall,
   OpenPlugin(usize),
   /// A plugin's code page, and saving what it holds.
   EditCode(usize),
   SaveCode(usize),
   PickFile,
   Toggle(usize),
   Reload(usize),
   Uninstall(usize),
   /// A plugin's switch or button, by its index in [`PluginInfo::settings`].
   Setting(usize, usize),
   /// Option `option` of a plugin's select setting.
   Choose(usize, usize, usize),
   /// Apply the pending update in-app (OTA plugins / APK self-updater).
   ApplyUpdate,
   /// The site, to get the update [`crate::update`] found.
   OpenSite,
   ToggleStaff,
   ToggleRemoteShell,
   Back,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PageKind {
   Main,
   Console,
   Install,
   Plugin(usize),
   Code(usize),
}

/// What the UI thread hands to the worker. UI ops must not be sent from the
/// main thread, where Rust may hold the UI lock while it waits on Java.
enum Job {
   Press(Press),
   NavigateTo(usize),
   /// A text field's contents after an edit.
   Draft(Field, String),
   Submit(Field),
   Log,
   /// The file picker's answer.
   Picked(crate::picker::Picked),
   /// Something the open pages show changed.
   Refresh,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Field {
   Eval,
   Install,
   /// A text or number setting, indexed like [`Press::Setting`].
   Setting(usize, usize),
   /// A plugin's code, by its index in [`crate::plugins`].
   Code(usize),
}

/// Nodes updated in place, so typing is not interrupted.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Slot {
   Input(Field),
   Log,
}

struct Tree {
   id:      i32,
   kind:    ViewKind,
   props:   Props,
   kids:    Vec<Self>,
   src:     Option<u32>,
   press:   Option<Press>,
   slot:    Option<Slot>,
   animate: Option<Animate>,
}

/// A node the host animates between two prop sets, like a switch's knob.
struct Animate {
   worklet: &'static Worklet,
   /// The props of the other state.
   other:   Props,
   /// Set when the node starts at `other` and springs to its own props.
   flipped: bool,
}

impl Tree {
   fn from_template(node: &Node) -> Self {
      Self {
         id:      0,
         kind:    node.kind.clone(),
         props:   node.props.clone(),
         kids:    node.kids.iter().map(Self::from_template).collect(),
         src:     node.src,
         press:   None,
         slot:    None,
         animate: None,
      }
   }

   fn set(&mut self, key: Prop, value: Option<Value>) {
      set_prop(&mut self.props, key, value);
   }

   fn find(&mut self, src: u32) -> Option<&mut Self> {
      if self.src == Some(src) {
         return Some(self);
      }
      self.kids.iter_mut().find_map(|kid| kid.find(src))
   }

   fn slots(&self) -> Vec<(i32, Slot)> {
      let mut slots = self
         .slot
         .map(|slot| (self.id, slot))
         .into_iter()
         .collect::<Vec<_>>();
      slots.extend(self.kids.iter().flat_map(Self::slots));
      slots
   }

   /// Marks the animated nodes under the one `press` targets as just flipped.
   fn flip(&mut self, press: Press, inside: bool) {
      let inside = inside || (self.press == Some(press) && self.animate.is_some());
      if inside && let Some(animate) = &mut self.animate {
         animate.flipped = true;
      }
      for kid in &mut self.kids {
         kid.flip(press, inside);
      }
   }

   fn ids(&self) -> Vec<i32> {
      let mut ids = vec![self.id];
      ids.extend(self.kids.iter().flat_map(Self::ids));
      ids
   }
}

/// Whether `new` can update `old` in place: the same nodes, handlers and
/// animations, and no prop dropped (an update can only set props).
fn same_shape(old: &Tree, new: &Tree) -> bool {
   old.kind == new.kind
      && old.press == new.press
      && old.slot == new.slot
      && match (&old.animate, &new.animate) {
         (None, None) => old.props.iter().all(|(key, _)| new.props.iter().any(|(k, _)| k == key)),
         (Some(a), Some(b)) => std::ptr::eq(a.worklet, b.worklet),
         _ => false,
      }
      && old.kids.len() == new.kids.len()
      && old.kids.iter().zip(&new.kids).all(|(a, b)| same_shape(a, b))
}

fn text(value: &str) -> Value {
   Value::Str(value.to_owned())
}

/// Motion slots for animated nodes, reused once a node is deleted.
struct SlotPool {
   next:  i32,
   free:  Vec<(i32, i32)>,
   owned: HashMap<i32, (i32, i32)>,
}

impl SlotPool {
   /// The first of `count` slots for `node`, and whether they were used
   /// before.
   fn take(&mut self, node: i32, count: i32) -> (i32, bool) {
      let (first, reused) = match self.free.iter().position(|&(_, size)| size == count) {
         Some(at) => (self.free.swap_remove(at).0, true),
         None => {
            self.next -= count;
            (self.next, false)
         },
      };
      self.owned.insert(node, (first, count));
      (first, reused)
   }

   fn release(&mut self, node: i32) {
      if let Some(block) = self.owned.remove(&node) {
         self.free.push(block);
      }
   }
}

/// The "Phoenix Unlocker" row in the Settings list.
struct Entry {
   root: i32,
   ids:  Vec<i32>,
}

struct Page {
   root:     i32,
   ids:      Vec<i32>,
   kind:     PageKind,
   content:  i32,
   sections: Vec<i32>,
   slots:    Vec<(i32, Slot)>,
   /// The built sections, to update in place when a rerender keeps their shape.
   trees:    Vec<Tree>,
}

/// The open Settings screen and what phoenix added to it.
struct Screen {
   list:       i32,
   /// Rust's own rows in the list and pages on the navigator, kept current
   /// from its batches.
   rust_rows:  Option<Vec<i32>>,
   search:     Option<i32>,
   navigator:  Option<i32>,
   rust_pages: Option<Vec<i32>>,
   header:     Option<Props>,
   entry:      Option<Entry>,
   stack:      Vec<Page>,
}

struct Settings {
   next_id:    i32,
   slots:      SlotPool,
   /// Motion words for nodes built since the last batch went out. They run
   /// after the batch attaches those nodes.
   motion:     Vec<i32>,
   navigators: HashSet<i32>,
   screen:     Option<Screen>,
   handlers:   HashMap<i32, Press>,
   plugins:    Vec<PluginInfo>,
   drafts:     HashMap<Field, String>,
   /// The line nodes in each console log box, replaced on every refresh.
   log_rows:   HashMap<i32, Vec<i32>>,
   /// A counter for `forceValue`, which the host applies when it changes.
   force_seq:  i64,
   /// What the last install did, shown on the install page.
   install_status: Option<(String, i64)>,
}

static SETTINGS: LazyLock<Mutex<Settings>> = LazyLock::new(|| {
   Mutex::new(Settings {
      next_id:    FIRST_ID,
      slots:      SlotPool {
         next:  SLOT_END,
         free:  Vec::new(),
         owned: HashMap::new(),
      },
      motion:     Vec::new(),
      navigators: HashSet::new(),
      screen:     None,
      handlers:   HashMap::new(),
      plugins:    Vec::new(),
      drafts:     HashMap::new(),
      log_rows:   HashMap::new(),
      force_seq:  1 << 40,
      install_status: None,
   })
});

static WORKER: OnceLock<Sender<Job>> = OnceLock::new();
/// Set while a [`Job::Log`] is queued, so a burst of lines costs one update.
static LOG_QUEUED: AtomicBool = AtomicBool::new(false);

fn settings() -> MutexGuard<'static, Settings> {
   SETTINGS.lock().unwrap_or_else(PoisonError::into_inner)
}

struct SettingsSink;

impl Sink for SettingsSink {
   fn ops(&self, batch: &[u8]) -> Action {
      rewrite_ops(batch)
   }

   fn dispatch(&self, frame: &[u8]) -> Action {
      filter_dispatch(frame)
   }
}

/// Hooks the UI bridge and starts the worker that answers presses.
pub fn start() {
   let (sender, receiver) = mpsc::channel::<Job>();
   if WORKER.set(sender).is_err() {
      log::warn!("settings: already started");
      return;
   }
   bridge::install(Box::new(SettingsSink));
   thread::spawn(move || {
      for job in receiver {
         act(&job);
         let plugins = matches!(
            job,
            Job::Press(_) | Job::Submit(_) | Job::Picked(_) | Job::Refresh
         )
         .then(crate::plugins);
         let ops = {
            let mut state = settings();
            if let Some(plugins) = plugins {
               state.plugins = plugins;
            }
            let mut ops = state.run(job);
            if !state.motion.is_empty() {
               ops.push(Op::Motion {
                  words: std::mem::take(&mut state.motion),
               });
            }
            ops
         };
         if !ops.is_empty() && !bridge::apply_ops(&encode(&ops)) {
            log::warn!("settings: the UI update did not apply");
         }
      }
   });
   crate::update::check();
}

/// Rebuilds the open pages, for state that changed outside them.
pub fn refresh() {
   if let Some(worker) = WORKER.get() {
      let _ = worker.send(Job::Refresh);
   }
}

/// Makes the runtime calls `job` asks for. They take the engine lock, so they
/// run before the settings lock is taken.
fn act(job: &Job) {
   let plugin = |index: usize| settings().plugins.get(index).cloned();
   match *job {
      Job::Log => LOG_QUEUED.store(false, Ordering::Relaxed),
      // The editor is multiline: Enter is a newline there, and Save saves.
      Job::Submit(Field::Code(_)) => {},
      Job::Submit(field) => {
         let Some(draft) = settings().drafts.remove(&field) else {
            return;
         };
         let input = draft.trim();
         match field {
            Field::Eval if !input.is_empty() => crate::evaluate(input),
            Field::Install if !input.is_empty() => {
               installed(crate::install(input), input);
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
            Field::Eval | Field::Install | Field::Code(_) => {},
         }
      },
      Job::Press(Press::PickFile) => {
         if let Err(err) = crate::picker::pick() {
            log::error!("file picker: {err}");
            settings().install_status = Some((format!("The file picker failed: {err}"), COLOR_ERROR));
         }
      },
      Job::Picked(ref picked) => {
         match picked {
            Ok(Some((file, bytes))) => installed(crate::install_file(file, bytes.clone()), file),
            Ok(None) => {},
            Err(err) => {
               log::error!("file picker: {err}");
               settings().install_status = Some((format!("The file picker failed: {err}"), COLOR_ERROR));
            },
         }
      },
      // The editor starts from the plugin's code, kept as the field's draft.
      Job::Press(Press::EditCode(index)) => {
         if let Some(plugin) = plugin(index)
            && let Some(source) = crate::source(&plugin.name)
         {
            settings().drafts.insert(Field::Code(index), source);
         }
      },
      Job::Press(Press::SaveCode(index)) => {
         let draft = settings().drafts.get(&Field::Code(index)).cloned();
         if let (Some(plugin), Some(source)) = (plugin(index), draft) {
            match crate::save_source(&plugin.name, source) {
               Ok(()) => log::info!("saved {} and restarted it", plugin.name),
               Err(err) => log::error!("saving {} failed: {err}", plugin.name),
            }
         }
      },
      Job::Press(Press::Toggle(index)) => {
         if let Some(plugin) = plugin(index) {
            crate::set_enabled(&plugin.name, !plugin.enabled);
         }
      },
      Job::Press(Press::Reload(index)) => {
         if let Some(plugin) = plugin(index) {
            crate::reload(&plugin.name);
         }
      },
      Job::Press(Press::Uninstall(index)) => {
         if let Some(plugin) = plugin(index) {
            match crate::uninstall(&plugin.name) {
               Ok(()) => log::info!("uninstalled {}", plugin.name),
               Err(err) => log::error!("uninstall failed: {err}"),
            }
         }
      },
      Job::Press(Press::Setting(index, at)) => {
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
      Job::Press(Press::Choose(index, at, option)) => {
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
      Job::Press(Press::ApplyUpdate) => {
         crate::update::apply();
      },
      Job::Press(Press::OpenSite) => {
         if let Err(err) = crate::picker::open(crate::update::SITE) {
            log::error!("opening {}: {err}", crate::update::SITE);
         }
      },
      Job::Press(Press::ToggleStaff) => {
         crate::set_staff_enabled(!crate::staff_enabled());
      },
      Job::Press(Press::ToggleRemoteShell) => {
         crate::set_remote_shell_enabled(!crate::remote_shell_enabled());
      },
      Job::Press(_) | Job::NavigateTo(_) | Job::Draft(..) | Job::Refresh => {},
   }
}

/// Logs an install's outcome and keeps it for the install page.
fn installed(result: Result<String, crate::Error>, from: &str) {
   let status = match result {
      Ok(name) => {
         log::info!("installed {name} from {from}");
         (format!("Installed {name}. It is on the {TITLE} page now."), COLOR_STRING)
      },
      Err(err) => {
         log::error!("install failed: {err}");
         (format!("Install failed: {err}"), COLOR_ERROR)
      },
   };
   settings().install_status = Some(status);
}

/// Refreshes the console page's log, if one is open.
pub fn log_changed() {
   if LOG_QUEUED.swap(true, Ordering::Relaxed) {
      return;
   }
   if let Some(worker) = WORKER.get()
      && worker.send(Job::Log).is_err()
   {
      LOG_QUEUED.store(false, Ordering::Relaxed);
   }
}

/// Splices phoenix's nodes into a Rust-to-Java UI batch.
fn rewrite_ops(batch: &[u8]) -> Action {
   // Most batches carry neither the Settings list nor a navigator, so they
   // pass without being decoded.
   let relevant = settings().screen.is_some()
      || memmem::find(batch, LOGOUT_ROW.as_bytes()).is_some()
      || memmem::find(batch, ViewKind::Navigator.as_str().as_bytes()).is_some();
   if !relevant {
      return Action::Pass;
   }
   let ops = match decode(batch) {
      Ok(ops) => ops,
      Err(err) => {
         log::warn!("settings: undecodable batch: {err}");
         return Action::Pass;
      },
   };
   settings()
      .rewrite(ops)
      .map_or(Action::Pass, |ops| Action::Replace(encode(&ops)))
}

/// Hands the file picker's answer to the worker.
pub fn picked(picked: crate::picker::Picked) {
   if let Some(worker) = WORKER.get()
      && worker.send(Job::Picked(picked)).is_err()
   {
      log::warn!("settings: the worker stopped");
   }
}

/// Takes view events on phoenix's nodes out of a Java-to-Rust dispatch.
fn filter_dispatch(frame: &[u8]) -> Action {
   let Ok(items) = decode_dispatch(frame) else {
      return Action::Pass;
   };
   let mut taken = Vec::new();
   let mut jobs = Vec::new();
   {
      let state = settings();
      let Some(screen) = &state.screen else {
         return Action::Pass;
      };
      for item in &items {
         let DispatchEvent::View {
            node,
            event,
            payload,
         } = &item.event
         else {
            continue;
         };
         let job = if *event == Prop::NAVIGATE.key()
            && !screen.stack.is_empty()
            && Some(*node) == screen.navigator
         {
            let index = payload
               .field(Prop::INDEX.key())
               .and_then(Value::as_i32)
               .and_then(|index| usize::try_from(index).ok());
            Some(Job::NavigateTo(
               index.unwrap_or_else(|| state.page_index().saturating_sub(1)),
            ))
         } else if let Some(field) = state.field(*node) {
            if *event == Prop::CHANGE_TEXT.key() {
               text_of(payload).map(|text| Job::Draft(field, text))
            } else {
               (*event == Prop::SUBMIT.key()).then_some(Job::Submit(field))
            }
         } else if let Some(press) = state.handlers.get(node) {
            (*event == Prop::PRESS.key()).then_some(Job::Press(*press))
         } else {
            continue;
         };
         taken.push(item.offset);
         jobs.extend(job);
      }
   }

   if let Some(worker) = WORKER.get() {
      for job in jobs {
         if worker.send(job).is_err() {
            log::warn!("settings: the worker stopped");
            break;
         }
      }
   }
   match (taken.len(), frame.first()) {
      (0, _) => Action::Pass,
      (count, _) if count == items.len() => Action::Drop,
      (_, Some(4)) => Action::Replace(without_frames(frame, &taken)),
      _ => Action::Drop,
   }
}

/// A `changeText` payload is the text itself or an object holding it.
fn text_of(payload: &Value) -> Option<String> {
   match payload {
      Value::Str(text) => Some(text.clone()),
      Value::Obj(_) => {
         [Prop::TEXT, Prop::VALUE]
            .into_iter()
            .find_map(|prop| payload.field(prop.key())?.as_str().map(str::to_owned))
      },
      _ => {
         log::warn!("settings: unexpected changeText payload {payload:?}");
         None
      },
   }
}

impl Settings {
   fn page_index(&self) -> usize {
      self.screen.as_ref().map_or(0, |screen| {
         (screen.rust_pages.as_ref().map_or(0, Vec::len) + screen.stack.len()).saturating_sub(1)
      })
   }

   fn build(&mut self, tree: &mut Tree, inherited: Option<Press>, ops: &mut Vec<Op>) -> i32 {
      let press = tree.press.or(inherited);
      let kids = tree
         .kids
         .iter_mut()
         .map(|kid| self.build(kid, press, ops))
         .collect::<Vec<i32>>();
      tree.id = self.next_id;
      self.next_id += 1;
      if let Some(press) = press {
         self.handlers.insert(tree.id, press);
      }
      let mut props = tree.props.clone();
      let motion = tree.animate.as_ref().map(|animate| {
         let (first, reused) = self.slots.take(tree.id, animate.worklet.slot_count());
         let bound = animate.worklet.bind(first);
         props.push(bound.prop());
         let start = if animate.flipped {
            &animate.other
         } else {
            &tree.props
         };
         let mut commands = bound.rest(start);
         if reused {
            commands.extend(bound.pin(start, Spring::SWITCH));
         }
         if animate.flipped {
            commands.extend(bound.spring(start, &tree.props, Spring::SWITCH));
         }
         commands
      });
      ops.push(Op::Create {
         id: tree.id,
         kind: tree.kind.clone(),
         props,
      });
      if let Some(commands) = motion {
         self.motion.extend(motion::words(&commands));
      }
      if !kids.is_empty() {
         ops.push(Op::Children {
            id:  tree.id,
            ids: kids,
         });
      }
      tree.id
   }

   fn forget(&mut self, ids: &[i32]) {
      for id in ids {
         self.handlers.remove(id);
         self.slots.release(*id);
         self.log_rows.remove(id);
      }
   }

   fn navigator_ops(&self) -> Vec<Op> {
      let Some(screen) = &self.screen else {
         return Vec::new();
      };
      let Some(navigator) = screen.navigator else {
         return Vec::new();
      };
      let mut ids = screen.rust_pages.clone().unwrap_or_default();
      ids.extend(screen.stack.iter().map(|page| page.root));
      vec![Op::Children { id: navigator, ids }, Op::Update {
         id:    navigator,
         props: vec![(
            Prop::INDEX,
            Value::I32(i32::try_from(self.page_index()).unwrap_or(i32::MAX)),
         )],
      }]
   }

   fn slots(&self) -> impl Iterator<Item = (i32, Slot)> + '_ {
      self
         .screen
         .iter()
         .flat_map(|screen| &screen.stack)
         .flat_map(|page| page.slots.iter().copied())
   }

   fn field(&self, node: i32) -> Option<Field> {
      self.slots().find_map(|(id, slot)| {
         match slot {
            Slot::Input(field) if id == node => Some(field),
            _ => None,
         }
      })
   }

   /// Fills every open console's log box with the current lines.
   // ponytail: rebuilds all lines per refresh (refreshes coalesce through
   // LOG_QUEUED); prepend only the new lines if this ever shows up in a trace.
   fn log_ops(&mut self) -> Vec<Op> {
      let boxes = self
         .slots()
         .filter(|(_, slot)| *slot == Slot::Log)
         .map(|(id, _)| id)
         .collect::<Vec<i32>>();
      let mut ops = Vec::new();
      for log_box in boxes {
         let mut lines = log_lines();
         let ids = lines
            .iter_mut()
            .map(|line| self.build(line, None, &mut ops))
            .collect::<Vec<i32>>();
         let old = self.log_rows.insert(log_box, ids.clone()).unwrap_or_default();
         if let Some(page) = self.screen.as_mut().and_then(|screen| {
            screen
               .stack
               .iter_mut()
               .find(|page| page.slots.contains(&(log_box, Slot::Log)))
         }) {
            page.ids.retain(|id| !old.contains(id));
            page.ids.extend(&ids);
         }
         ops.push(Op::Children { id: log_box, ids });
         if !old.is_empty() {
            self.forget(&old);
            ops.push(Op::Delete { ids: old });
         }
      }
      ops
   }

   /// Empties the inputs that hold `field`. `forceValue` rather than `value`,
   /// which the host ignores while the keyboard has edits in flight.
   fn clear_ops(&mut self, field: Field) -> Vec<Op> {
      let inputs = self
         .slots()
         .filter(|(_, slot)| *slot == Slot::Input(field))
         .map(|(id, _)| id)
         .collect::<Vec<i32>>();
      let key = |name| Prop::named(name).key();
      inputs
         .into_iter()
         .map(|id| {
            self.force_seq += 1;
            Op::Update {
               id,
               props: vec![(
                  Prop::named("forceValue"),
                  Value::Obj(vec![
                     (key("seq"), Value::I64(self.force_seq)),
                     (key("text"), text("")),
                     (key("start"), Value::I32(0)),
                     (key("end"), Value::I32(0)),
                  ]),
               )],
            }
         })
         .collect()
   }

   fn run(&mut self, job: Job) -> Vec<Op> {
      match job {
         Job::Draft(field, draft) => {
            let ops = match field {
               Field::Code(_) => self
                  .slots()
                  .filter(|(_, slot)| *slot == Slot::Input(field))
                  .map(|(id, _)| Op::Update {
                     id,
                     props: vec![(Prop::named("valueSpans"), code_spans(&draft))],
                  })
                  .collect(),
               _ => Vec::new(),
            };
            self.drafts.insert(field, draft);
            ops
         },
         Job::Log => self.log_ops(),
         Job::Submit(Field::Eval) => {
            let mut ops = self.log_ops();
            ops.extend(self.clear_ops(Field::Eval));
            ops
         },
         Job::Submit(Field::Code(_)) | Job::Press(Press::PickFile | Press::OpenSite) => Vec::new(),
         Job::Refresh => self.rerender_all(None),
         Job::Press(Press::OpenMain) => self.open_page(PageKind::Main),
         Job::Press(Press::OpenConsole) => {
            let mut ops = self.open_page(PageKind::Console);
            ops.extend(self.log_ops());
            ops
         },
         Job::Press(Press::OpenPlugin(index)) => self.open_page(PageKind::Plugin(index)),
         Job::Press(Press::OpenInstall) => {
            self.install_status = None;
            self.open_page(PageKind::Install)
         },
         Job::Press(Press::EditCode(index)) => self.open_page(PageKind::Code(index)),
         Job::Press(Press::SaveCode(_)) | Job::Picked(_) => self.rerender_all(None),
         Job::Press(
            press @ (Press::Toggle(_)
            | Press::Setting(..)
            | Press::Choose(..)
            | Press::ToggleStaff
            | Press::ToggleRemoteShell),
         ) => {
            self.rerender_all(Some(press))
         },
         Job::Submit(Field::Install | Field::Setting(..))
         | Job::Press(Press::Reload(_) | Press::ApplyUpdate) => self.rerender_all(None),
         Job::Press(Press::Back) => self.close_top(),
         Job::Press(Press::Uninstall(_)) => {
            let mut ops = self.close_top();
            ops.extend(self.rerender_all(None));
            ops
         },
         Job::NavigateTo(index) => {
            let mut ops = Vec::new();
            while self
               .screen
               .as_ref()
               .is_some_and(|screen| !screen.stack.is_empty())
               && self.page_index() > index
            {
               ops.extend(self.close_top());
            }
            ops
         },
      }
   }

   fn render(&self, kind: PageKind) -> (String, Vec<Tree>) {
      match kind {
         PageKind::Main => (TITLE.to_owned(), main_sections(&self.plugins)),
         PageKind::Console => ("Console".to_owned(), console_sections()),
         PageKind::Install => ("Install plugin".to_owned(), install_sections(self.install_status.as_ref())),
         PageKind::Code(index) => {
            let source = self.drafts.get(&Field::Code(index)).map(String::as_str);
            let name = self.plugins.get(index).map_or("plugin", |plugin| plugin.name.as_str());
            ("Edit code".to_owned(), code_sections(index, name, source))
         },
         PageKind::Plugin(index) => {
            self.plugins.get(index).map_or_else(
               || ("Plugin".to_owned(), Vec::new()),
               |plugin| (plugin.name.clone(), plugin_sections(index, plugin)),
            )
         },
      }
   }

   fn build_sections(&mut self, sections: &mut [Tree], ops: &mut Vec<Op>) -> (Vec<i32>, Vec<i32>) {
      let roots = sections
         .iter_mut()
         .map(|section| self.build(section, None, ops))
         .collect();
      let ids = sections.iter().flat_map(Tree::ids).collect();
      (roots, ids)
   }

   /// Rebuilds every open page. The switches `flipped` targets spring from
   /// their old state.
   fn rerender_all(&mut self, flipped: Option<Press>) -> Vec<Op> {
      let kinds = self.screen.as_ref().map_or_default(|screen| {
         screen
            .stack
            .iter()
            .map(|page| page.kind)
            .collect::<Vec<PageKind>>()
      });
      let mut ops = Vec::new();
      for (position, kind) in kinds.into_iter().enumerate() {
         // Rebuilding these would drop what is being typed into them.
         if matches!(kind, PageKind::Console | PageKind::Code(_)) {
            continue;
         }
         let (_, mut sections) = self.render(kind);
         // Same layout: update the changed nodes and spring the flipped switch,
         // so the other rows are not rebuilt (and do not flicker).
         let old = self
            .screen
            .as_mut()
            .and_then(|screen| screen.stack.get_mut(position))
            .map(|page| std::mem::take(&mut page.trees))
            .unwrap_or_default();
         if old.len() == sections.len() && old.iter().zip(&sections).all(|(a, b)| same_shape(a, b)) {
            for (old, new) in old.iter().zip(&mut sections) {
               self.patch(old, new, &mut ops);
            }
            if let Some(page) = self.screen.as_mut().and_then(|screen| screen.stack.get_mut(position)) {
               page.trees = sections;
            }
            continue;
         }
         if let Some(press) = flipped {
            for section in &mut sections {
               section.flip(press, false);
            }
         }
         let (roots, ids) = self.build_sections(&mut sections, &mut ops);
         let Some(page) = self
            .screen
            .as_mut()
            .and_then(|screen| screen.stack.get_mut(position))
         else {
            continue;
         };
         page.slots = sections.iter().flat_map(Tree::slots).collect();
         page.trees = sections;
         let old = std::mem::replace(&mut page.sections, ids.clone());
         page.ids.retain(|id| !old.contains(id));
         page.ids.extend(ids);
         ops.push(Op::Children {
            id:  page.content,
            ids: roots,
         });
         ops.push(Op::Delete { ids: old.clone() });
         self.forget(&old);
      }
      ops
   }

   /// Gives `new` the ids of `old` (same shape) and sends what changed.
   fn patch(&mut self, old: &Tree, new: &mut Tree, ops: &mut Vec<Op>) {
      new.id = old.id;
      if let Some(animate) = &new.animate {
         if old.props != new.props
            && let Some(&(first, _)) = self.slots.owned.get(&old.id)
         {
            let commands = animate.worklet.bind(first).spring(&old.props, &new.props, Spring::SWITCH);
            self.motion.extend(motion::words(&commands));
         }
      } else {
         let changed = new
            .props
            .iter()
            .filter(|(key, value)| !old.props.iter().any(|(k, v)| k == key && v == value))
            .cloned()
            .collect::<Props>();
         if !changed.is_empty() {
            ops.push(Op::Update {
               id:    old.id,
               props: changed,
            });
         }
      }
      for (old, new) in old.kids.iter().zip(&mut new.kids) {
         self.patch(old, new, ops);
      }
   }

   fn open_page(&mut self, kind: PageKind) -> Vec<Op> {
      let Some(screen) = &self.screen else {
         return Vec::new();
      };
      if screen.rust_pages.is_none() {
         log::warn!("settings: no navigator pages known yet");
         return Vec::new();
      }
      let header = screen.header.clone();
      let (title, sections) = self.render(kind);
      let title = title.as_str();

      let mut page = Tree::from_template(&templates().page);
      page.set(Prop::PANE_TITLE, Some(text(title)));
      if let Some(title_node) = page.find(PAGE_TITLE) {
         title_node.set(Prop::TEXT, Some(text(title)));
      }
      if let Some(header) = header {
         let value = |key: Prop| {
            header
               .iter()
               .find(|(existing, _)| *existing == key)
               .map(|(_, value)| value.clone())
         };
         let height = value(Prop::HEIGHT);
         if let Some(node) = page.find(PAGE_HEADER) {
            for key in [Prop::PADDING, Prop::HEIGHT] {
               if let Some(found) = value(key) {
                  node.set(key, Some(found));
               }
            }
         }
         if let (Some(node), Some(height)) = (page.find(PAGE_HEADER_SPACER), height) {
            node.set(Prop::HEIGHT, Some(height));
         }
      }
      if let Some(back) = page.find(PAGE_BACK) {
         back.set(Prop::ACCESSIBILITY_LABEL, Some(text("Back")));
         back.press = Some(Press::Back);
      }
      if let Some(content) = page.find(PAGE_CONTENT) {
         content.kids = sections;
      }

      let mut ops = Vec::new();
      let root = self.build(&mut page, None, &mut ops);
      let ids = page.ids();
      let slots = page.slots();
      let Some(content) = page.find(PAGE_CONTENT) else {
         log::warn!("settings: the page layout has no content node");
         return Vec::new();
      };
      let sections = content.kids.iter().flat_map(Tree::ids).collect();
      let trees = std::mem::take(&mut content.kids);
      let content = content.id;
      if let Some(screen) = &mut self.screen {
         screen.stack.push(Page {
            root,
            ids,
            kind,
            content,
            sections,
            slots,
            trees,
         });
      }
      ops.extend(self.navigator_ops());
      ops
   }

   fn close_top(&mut self) -> Vec<Op> {
      let Some(page) = self.screen.as_mut().and_then(|screen| screen.stack.pop()) else {
         return Vec::new();
      };
      self.forget(&page.ids);
      let mut ops = self.navigator_ops();
      ops.push(Op::Delete { ids: page.ids });
      ops
   }

   fn entry_ops(&mut self) -> (Entry, Vec<Op>) {
      let row = button_row(TITLE, ICON_PLUGIN, Some(Press::OpenMain), true);
      let mut section = section("", vec![row]);
      let mut ops = Vec::new();
      let root = self.build(&mut section, None, &mut ops);
      (
         Entry {
            root,
            ids: section.ids(),
         },
         ops,
      )
   }

   fn rewrite(&mut self, ops: Vec<Op>) -> Option<Vec<Op>> {
      let mut creates = HashMap::new();
      let mut parents = HashMap::new();
      for op in &ops {
         match op {
            Op::Create { id, kind, props } => {
               if *kind == ViewKind::Navigator {
                  self.navigators.insert(*id);
               }
               creates.insert(*id, (kind, props));
            },
            Op::Children { id, ids } => {
               for kid in ids {
                  parents.insert(*kid, *id);
               }
            },
            _ => {},
         }
      }
      self.find_screen(&creates, &parents);
      self.screen.as_ref()?;
      let stacks = creates
         .iter()
         .filter(|(_, (kind, _))| **kind == ViewKind::Stack)
         .map(|(id, _)| *id)
         .collect::<HashSet<i32>>();
      drop(creates);

      let mut changed = false;
      let mut out = Vec::with_capacity(ops.len());
      let mut rest = ops.into_iter();
      while let Some(op) = rest.next() {
         let screen = self.screen.as_ref().expect("checked above");
         let (list, navigator) = (screen.list, screen.navigator);
         match op {
            Op::Children { id, ids } if id == list => {
               let search = ids.first().copied().filter(|first| stacks.contains(first));
               self.list_changed(ids, search, &mut out);
               changed = true;
            },
            Op::ChildEdits { id, edits } if id == list && screen.rust_rows.is_some() => {
               let ids = apply_edits(screen.rust_rows.as_deref().unwrap_or_default(), &edits);
               let search = screen.search;
               self.list_changed(ids, search, &mut out);
               changed = true;
            },
            Op::Children { id, ids } if Some(id) == navigator => {
               let original = Op::Children {
                  id,
                  ids: ids.clone(),
               };
               changed |= self.navigator_changed(original, ids, &mut out);
            },
            Op::ChildEdits { id, edits }
               if Some(id) == navigator && screen.rust_pages.is_some() =>
            {
               let ids = apply_edits(screen.rust_pages.as_deref().unwrap_or_default(), &edits);
               changed |= self.navigator_changed(Op::ChildEdits { id, edits }, ids, &mut out);
            },
            Op::Delete { mut ids }
               if ids.contains(&list)
                  || navigator.is_some_and(|navigator| ids.contains(&navigator)) =>
            {
               // Java's delete does not recurse, so phoenix's nodes are listed
               // here.
               let screen = self.screen.take().expect("checked above");
               let ours = screen
                  .entry
                  .into_iter()
                  .flat_map(|entry| entry.ids)
                  .chain(screen.stack.into_iter().flat_map(|page| page.ids))
                  .collect::<Vec<i32>>();
               self.forget(&ours);
               ids.extend(ours);
               out.push(Op::Delete { ids });
               out.extend(rest.by_ref());
               changed = true;
            },
            other => out.push(other),
         }
      }
      changed.then_some(out)
   }

   fn find_screen(
      &mut self,
      creates: &HashMap<i32, (&ViewKind, &Props)>,
      parents: &HashMap<i32, i32>,
   ) {
      for (id, (_, props)) in creates {
         let is_logout = props.iter().any(|(key, value)| {
            *key == Prop::ACCESSIBILITY_ID
               && matches!(value, Value::Str(found) if found == LOGOUT_ROW)
         });
         if !is_logout {
            continue;
         }
         let Some(list) = parents
            .get(id)
            .and_then(|row| parents.get(row))
            .and_then(|group| parents.get(group))
            .copied()
         else {
            continue;
         };
         if self
            .screen
            .as_ref()
            .is_some_and(|screen| screen.list == list)
         {
            continue;
         }

         let presented = |node: &i32| {
            creates.get(node).is_some_and(|(_, props)| {
               props
                  .iter()
                  .any(|(key, _)| *key == Prop::PRESENTATION || *key == Prop::UIKIT_PUSH)
            })
         };
         let mut navigator = None;
         let mut cursor = Some(list);
         while let Some(node) = cursor {
            if self.navigators.contains(&node) {
               navigator = Some(node);
               break;
            }
            if presented(&node) {
               navigator = parents.get(&node).copied();
               break;
            }
            cursor = parents.get(&node).copied();
         }
         let header = creates
            .values()
            .find(|(_, props)| props.iter().any(|(key, _)| *key == Prop::STACK_HEADER))
            .map(|(_, props)| (*props).clone());
         self.screen = Some(Screen {
            list,
            rust_rows: None,
            search: None,
            navigator,
            rust_pages: None,
            header,
            entry: None,
            stack: Vec::new(),
         });
      }
   }

   fn list_changed(&mut self, ids: Vec<i32>, search: Option<i32>, out: &mut Vec<Op>) {
      if self
         .screen
         .as_ref()
         .is_some_and(|screen| screen.entry.is_none())
      {
         let (entry, ops) = self.entry_ops();
         out.extend(ops);
         if let Some(screen) = &mut self.screen {
            screen.entry = Some(entry);
         }
      }
      let Some(screen) = &mut self.screen else {
         return;
      };
      if screen.search.is_none() {
         screen.search = search;
      }
      let at = usize::from(screen.search.is_some() && ids.first() == screen.search.as_ref());
      let mut with_entry = ids.clone();
      if let Some(entry) = &screen.entry {
         with_entry.insert(at, entry.root);
      }
      screen.rust_rows = Some(ids);
      out.push(Op::Children {
         id:  screen.list,
         ids: with_entry,
      });
   }

   /// Records Rust's pages on the navigator. While phoenix has pages open,
   /// they stay on top of Rust's.
   fn navigator_changed(&mut self, original: Op, ids: Vec<i32>, out: &mut Vec<Op>) -> bool {
      let Some(screen) = &mut self.screen else {
         out.push(original);
         return false;
      };
      let (Some(navigator), false) = (screen.navigator, screen.stack.is_empty()) else {
         screen.rust_pages = Some(ids);
         out.push(original);
         return false;
      };
      let mut all = ids.clone();
      all.extend(screen.stack.iter().map(|page| page.root));
      screen.rust_pages = Some(ids);
      out.push(Op::Children {
         id:  navigator,
         ids: all,
      });
      true
   }
}

fn apply_edits(old: &[i32], edits: &[ChildEdit]) -> Vec<i32> {
   let mut out = Vec::new();
   let mut cursor = 0;
   for edit in edits {
      let keep = usize::try_from(edit.keep).unwrap_or(0);
      let del = usize::try_from(edit.del).unwrap_or(0);
      out.extend(old.iter().skip(cursor).take(keep));
      cursor += keep + del;
      out.extend(&edit.ins);
   }
   out.extend(old.iter().skip(cursor));
   out
}

fn is_radius(key: Prop) -> bool {
   RADII.contains(&key)
}

fn section(title: &str, rows: Vec<Tree>) -> Tree {
   let layouts = templates();
   let mut section = Tree::from_template(&layouts.section);
   let mut group = Tree::from_template(&layouts.group);
   group.kids = rows
      .into_iter()
      .enumerate()
      .flat_map(|(index, row)| {
         let divider = (index > 0).then(|| Tree::from_template(&layouts.divider));
         divider.into_iter().chain([row])
      })
      .collect();
   if !title.is_empty() {
      let mut header = Tree::from_template(&layouts.header);
      header.set(Prop::TEXT, Some(text(title)));
      section.kids.push(header);
   }
   section.kids.push(group);
   section
}

/// `root > row > [icon, text, chevron]`
fn button_row(title: &str, icon: &str, press: Option<Press>, chevron: bool) -> Tree {
   let mut root = Tree::from_template(&templates().button_row);
   root.props.retain(|(key, _)| !is_radius(*key));
   root.set(Prop::ACCESSIBILITY_LABEL, Some(text(title)));
   root.set(
      Prop::ACCESSIBILITY_ID,
      Some(text(&format!("phoenix-{title}"))),
   );
   if let Some(row) = root.kids.first_mut() {
      if let Some(icon_node) = row.kids.get_mut(0) {
         icon_node.set(Prop::URI, Some(text(icon)));
      }
      if let Some(label) = row.kids.get_mut(1) {
         label.set(Prop::TEXT, Some(text(title)));
      }
      if !chevron {
         row.kids.truncate(2);
      }
   }
   if press.is_none() {
      root.set(Prop::ON, None);
      root.set(Prop::ROLE, None);
   }
   root.press = press;
   root
}

/// A button row without an icon, with a checkmark in the chevron's place when
/// `selected`.
fn select_row(label: &str, selected: bool, press: Press) -> Tree {
   let mut root = button_row(label, ICON_PLUGIN, Some(press), selected);
   if let Some(row) = root.kids.first_mut() {
      row.kids.remove(0);
      if let Some(check) = row
         .kids
         .get_mut(1)
         .and_then(|wrap| wrap.kids.first_mut())
      {
         check.set(Prop::URI, Some(text(ICON_CHECK)));
      }
   }
   root.set(Prop::ACCESSIBILITY_CHECKED, Some(Value::Bool(selected)));
   root
}

/// `root > row > [label, value wrap > value, chevron]`
fn value_row(label: &str, value: &str) -> Tree {
   let mut root = Tree::from_template(&templates().value_row);
   root.props.retain(|(key, _)| !is_radius(*key));
   root.set(Prop::ON, None);
   root.set(Prop::ROLE, None);
   root.set(
      Prop::ACCESSIBILITY_LABEL,
      Some(text(&format!("{label}, {value}"))),
   );
   if let Some(row) = root.kids.first_mut() {
      if let Some(label_node) = row.kids.get_mut(0) {
         label_node.set(Prop::TEXT, Some(text(label)));
      }
      if let Some(value_node) = row
         .kids
         .get_mut(1)
         .and_then(|wrap| wrap.kids.get_mut(0))
         .and_then(|inner| inner.kids.get_mut(0))
      {
         value_node.set(Prop::TEXT, Some(text(value)));
      }
      row.kids.truncate(2);
   }
   root
}

/// `root > row > [icon, column > [title, subtitle], wrap > track > knob]`.
/// Pressing the row runs `press`, pressing the switch runs `toggle`.
fn switch_row(
   icon: Option<&str>,
   title: &str,
   subtitle: Option<&str>,
   on: bool,
   press: Press,
   toggle: Press,
) -> Tree {
   let layouts = templates();
   let mut root = Tree::from_template(&layouts.switch_row);
   root.props.retain(|(key, _)| !is_radius(*key));
   let label = subtitle.map_or_else(
      || title.to_owned(),
      |subtitle| format!("{title}, {subtitle}"),
   );
   root.set(Prop::ACCESSIBILITY_LABEL, Some(text(&label)));
   root.set(Prop::ACCESSIBILITY_CHECKED, Some(Value::Bool(on)));
   root.set(
      Prop::ACCESSIBILITY_ID,
      Some(text(&format!("phoenix-{title}"))),
   );
   root.press = Some(press);
   let Some(row) = root.kids.first_mut() else {
      return root;
   };
   if let Some(icon_node) = row.kids.get_mut(0) {
      icon_node.set(Prop::URI, icon.map(text));
   }
   if let Some(column) = row.kids.get_mut(1) {
      if let Some(title_node) = column.kids.get_mut(0) {
         title_node.set(Prop::TEXT, Some(text(title)));
      }
      match subtitle {
         Some(subtitle) => {
            if let Some(subtitle_node) = column.kids.get_mut(1) {
               subtitle_node.set(Prop::TEXT, Some(text(subtitle)));
            }
         },
         None => column.kids.truncate(1),
      }
   }
   if let Some(track) = row.kids.get_mut(2).and_then(|wrap| wrap.kids.get_mut(0)) {
      track.press = Some(toggle);
      animate(track, &layouts.track_worklet, &layouts.track_off, on);
      if let Some(knob) = track.kids.get_mut(0) {
         animate(knob, &layouts.knob_worklet, &layouts.knob_off, on);
      }
   }
   if icon.is_none() {
      row.kids.remove(0);
   }
   root
}

/// Gives a switch part built from the "on" template the props for `on` and a
/// worklet to spring between both states.
fn animate(node: &mut Tree, worklet: &'static Worklet, off: &Props, on: bool) {
   let other = if on {
      off.clone()
   } else {
      std::mem::replace(&mut node.props, off.clone())
   };
   node.animate = Some(Animate {
      worklet,
      other,
      flipped: false,
   });
}

fn main_sections(plugins: &[PluginInfo]) -> Vec<Tree> {
   let rows = if plugins.is_empty() {
      vec![button_row("No plugins found", ICON_PLUGIN, None, false)]
   } else {
      plugins
         .iter()
         .enumerate()
         .map(|(index, plugin)| {
            let subtitle = plugin
               .error
               .as_deref()
               .map(|error| format!("Failed: {error}"))
               .or_else(|| plugin.header.description.clone());
            switch_row(
               Some(ICON_PLUGIN),
               &plugin.name,
               subtitle.as_deref(),
               plugin.enabled,
               Press::OpenPlugin(index),
               Press::Toggle(index),
            )
         })
         .collect()
   };
   let enabled = plugins.iter().filter(|plugin| plugin.enabled).count();
   let update_found = crate::update::found();
   let update_status = crate::update::status();
   let update_busy = crate::update::busy();
   let update = (update_found.is_some() || update_status.is_some()).then(|| {
      let mut update_rows = Vec::new();
      if let Some(update) = &update_found {
         let what = if update_busy {
            update_status
               .clone()
               .unwrap_or_else(|| "Updating...".to_owned())
         } else {
            match (&update.loader, update.plugins.len()) {
               (Some(version), 0) => format!("Install update ({version})"),
               (Some(version), n) => format!(
                  "Install update ({version}) + {n} plugin{}",
                  if n == 1 { "" } else { "s" }
               ),
               (None, _) => format!("Update plugins now: {}", update.plugins.join(", ")),
            }
         };
         let mut row = button_row(
            &what,
            ICON_RELOAD,
            (!update_busy).then_some(Press::ApplyUpdate),
            !update_busy,
         );
         if let Some(label) = row.kids.first_mut().and_then(|row| row.kids.get_mut(1)) {
            let color = if update_busy { COLOR_ACCENT } else { COLOR_STRING };
            label.set(Prop::COLOR, Some(Value::I64(color)));
         }
         update_rows.push(row);
      }
      if let Some(status) = &update_status
         && (!update_busy || update_found.is_none())
      {
         let mut line = log_line(vec![(status.clone(), COLOR_WARN)]);
         line.set(Prop::FONT_FAMILY, None);
         line.set(Prop::FONT_SIZE, Some(Value::F64(13.0)));
         line.set(Prop::LINE_HEIGHT, Some(Value::F64(18.0)));
         update_rows.push(line);
      }
      if update_found.is_some() {
         update_rows.push(button_row(
            "Open download page",
            ICON_FILE,
            Some(Press::OpenSite),
            true,
         ));
      }
      section("Update available", update_rows)
   });
   let debug_rows = vec![
      switch_row(
         None,
         "Staff unlock",
         Some("Grant the staff flag on READY/USER_UPDATE to unlock Discord experiments (restart to apply)"),
         crate::staff_enabled(),
         Press::ToggleStaff,
         Press::ToggleStaff,
      ),
      switch_row(
         None,
         "Remote debug shell",
         Some("Listen on 127.0.0.1:4242 for ADB eval, logs & plugin hot-reload (adb forward tcp:4242 tcp:4242)"),
         crate::remote_shell_enabled(),
         Press::ToggleRemoteShell,
         Press::ToggleRemoteShell,
      ),
   ];
   update.into_iter().chain([
      section("Plugins", rows),
      section("", vec![
         button_row("Install plugin", ICON_FILE, Some(Press::OpenInstall), true),
         button_row("Console", ICON_CONSOLE, Some(Press::OpenConsole), true),
      ]),
      section("Debug", debug_rows),
      section("About", vec![
         value_row("Version", crate::update::VERSION),
         value_row("Runtime", "rquickjs"),
         value_row("Gateway hooks", "registry pointer swap"),
         value_row("Hook library", HOOK_LIBRARY),
         value_row("Enabled", &format!("{enabled} of {}", plugins.len())),
      ]),
   ])
   .collect()
}

/// A colored piece of a text's `spans`.
fn span(piece: &str, color: i64) -> Value {
   Value::Obj(vec![
      (Prop::TEXT.key(), text(piece)),
      (Prop::COLOR.key(), Value::I64(color)),
   ])
}

fn code_color(kind: Kind) -> i64 {
   match kind {
      Kind::Plain => COLOR_TEXT,
      Kind::Comment => COLOR_COMMENT,
      Kind::String => COLOR_STRING,
      Kind::Keyword => COLOR_KEYWORD,
      Kind::Number => COLOR_NUMBER,
   }
}

/// `source` as highlighted spans.
fn code_spans(source: &str) -> Value {
   Value::Arr(
      highlight::js(source)
         .into_iter()
         .map(|(kind, run)| span(run, code_color(kind)))
         .collect(),
   )
}

/// The newest kept lines, colored by part: time, `[plugin]` tag, level.
fn log_lines() -> Vec<Tree> {
   let lines = crate::console::lines();
   if lines.is_empty() {
      return vec![log_line(vec![("Nothing logged yet".to_owned(), COLOR_MUTED)])];
   }
   lines.iter().take(LOG_LINES).map(|line| log_line(line_spans(line))).collect()
}

fn line_spans(line: &crate::console::Line) -> Vec<(String, i64)> {
   let mut spans = vec![(format!("{} ", line.time), COLOR_MUTED)];
   let body = match line.level {
      log::Level::Error => COLOR_ERROR,
      log::Level::Warn => COLOR_WARN,
      log::Level::Debug | log::Level::Trace => COLOR_MUTED,
      log::Level::Info => COLOR_TEXT,
   };
   let mut rest = line.text.as_str();
   // What the console ran, echoed as "> code": highlighted like the editor.
   if line.level == log::Level::Info
      && let Some(code) = rest.strip_prefix("> ")
   {
      spans.push(("> ".to_owned(), COLOR_ACCENT));
      spans.extend(
         highlight::js(code)
            .into_iter()
            .map(|(kind, run)| (run.to_owned(), code_color(kind))),
      );
      return spans;
   }
   if rest.starts_with('[')
      && let Some(end) = rest.find("] ")
   {
      spans.push((rest[..=end].to_owned(), COLOR_TAG));
      rest = &rest[end + 1..];
   }
   spans.push((rest.trim_end().to_owned(), body));
   spans
}

/// One small monospace console line, selectable for copying.
fn log_line(pieces: Vec<(String, i64)>) -> Tree {
   let mut node = Tree::from_template(&templates().header);
   node.set(Prop::ROLE, None);
   node.set(Prop::MARGIN, None);
   let whole = pieces.iter().map(|(piece, _)| piece.as_str()).collect::<String>();
   node.set(Prop::TEXT, Some(text(&whole)));
   node.set(
      Prop::named("spans"),
      Some(Value::Arr(pieces.iter().map(|(piece, color)| span(piece, *color)).collect())),
   );
   node.set(Prop::COLOR, Some(Value::I64(COLOR_TEXT)));
   node.set(Prop::FONT_SIZE, Some(Value::F64(12.0)));
   node.set(Prop::LINE_HEIGHT, Some(Value::F64(16.0)));
   node.set(Prop::named("fontWeight"), Some(Value::I32(400)));
   node.set(Prop::FONT_FAMILY, Some(text("monospace")));
   node.set(Prop::SELECTABLE, Some(Value::Bool(true)));
   node
}

/// What a text field takes, which picks its keyboard.
#[derive(Clone, Copy, PartialEq, Eq)]
enum InputKind {
   Text,
   Number,
   Secret,
}

/// `root > frame > row > [icon wrap > icon, input]`, Phoenix's search box.
fn text_field(
   field: Field,
   icon: Option<&str>,
   placeholder: &str,
   value: &str,
   entry: InputKind,
) -> Tree {
   let mut root = Tree::from_template(&templates().input);
   let Some(row) = root
      .kids
      .first_mut()
      .and_then(|frame| frame.kids.first_mut())
   else {
      return root;
   };
   if let Some(icon_node) = row.kids.first_mut().and_then(|wrap| wrap.kids.first_mut()) {
      icon_node.set(Prop::URI, icon.map(text));
   }
   if let Some(input) = row.kids.get_mut(1) {
      input.set(Prop::PLACEHOLDER, Some(text(placeholder)));
      input.set(Prop::VALUE, Some(text(value)));
      input.set(
         Prop::KEYBOARD,
         (entry == InputKind::Number).then(|| text("number")),
      );
      input.set(
         Prop::SECURE,
         (entry == InputKind::Secret).then_some(Value::Bool(true)),
      );
      input.set(Prop::RETURN_KEY, Some(text("go")));
      input.set(Prop::ROLE, None);
      input.set(
         Prop::ACCESSIBILITY_ID,
         Some(text(&format!("phoenix-{placeholder}"))),
      );
      input.slot = Some(Slot::Input(field));
   }
   if icon.is_none() {
      row.kids.remove(0);
      if let Some(input) = row.kids.first_mut() {
         input.set(Prop::MARGIN, None);
      }
   }
   root
}

fn install_sections(status: Option<&(String, i64)>) -> Vec<Tree> {
   let mut sections = vec![
      section("From a URL", vec![text_field(
         Field::Install,
         Some(ICON_PLUGIN),
         "https://example.com/plugin.js",
         "",
         InputKind::Text,
      )]),
      section("From a file", vec![button_row(
         "Choose a .js file",
         ICON_FILE,
         Some(Press::PickFile),
         false,
      )]),
   ];
   if let Some((message, color)) = status {
      let mut line = log_line(vec![(message.clone(), *color)]);
      line.set(Prop::FONT_FAMILY, None);
      line.set(Prop::FONT_SIZE, Some(Value::F64(14.0)));
      line.set(Prop::LINE_HEIGHT, Some(Value::F64(18.0)));
      sections.push(section("", vec![line]));
   }
   sections
}

fn console_sections() -> Vec<Tree> {
   let input_row = text_field(
      Field::Eval,
      Some(ICON_CONSOLE),
      "Evaluate JavaScript",
      "",
      InputKind::Text,
   );
   // A padded box the lines go into; [`Settings::log_ops`] fills it.
   let mut log_box = Tree::from_template(&templates().button_row);
   log_box.kids.clear();
   log_box.props.retain(|(key, _)| !is_radius(*key));
   for key in [Prop::ON, Prop::ROLE, Prop::ACCESSIBILITY_LABEL, Prop::ACCESSIBILITY_ID] {
      log_box.set(key, None);
   }
   log_box.set(Prop::PADDING, Some(Value::Rect([12.0, 10.0, 12.0, 10.0])));
   log_box.set(Prop::GAP, Some(Value::F64(2.0)));
   log_box.slot = Some(Slot::Log);
   vec![
      section("", vec![input_row]),
      section("Log, newest first", vec![log_box]),
   ]
}

/// The plugin's code in a multiline monospace field, and Save.
fn code_sections(index: usize, name: &str, source: Option<&str>) -> Vec<Tree> {
   let Some(source) = source.filter(|source| source.len() <= MAX_EDIT) else {
      let why = source.map_or_else(
         || "This plugin's code is not available".to_owned(),
         |source| format!("This plugin is {} KB, too large to edit here", source.len() / 1024),
      );
      return vec![section("", vec![button_row(&why, ICON_CONSOLE, None, false)])];
   };
   let mut editor = text_field(Field::Code(index), None, "", source, InputKind::Text);
   if let Some(input) = editor
      .kids
      .first_mut()
      .and_then(|frame| frame.kids.first_mut())
      .and_then(|row| row.kids.first_mut())
   {
      input.set(Prop::named("multiline"), Some(Value::Bool(true)));
      input.set(Prop::FONT_FAMILY, Some(text("monospace")));
      input.set(Prop::FONT_SIZE, Some(Value::F64(12.0)));
      input.set(Prop::named("minHeight"), Some(Value::F64(320.0)));
      input.set(Prop::RETURN_KEY, None);
      input.set(Prop::PLACEHOLDER, None);
      input.set(Prop::named("valueSpans"), Some(code_spans(source)));
   }
   vec![
      section(&format!("{name}.js"), vec![editor]),
      section("", vec![button_row(
         "Save and restart",
         ICON_RELOAD,
         Some(Press::SaveCode(index)),
         false,
      )]),
   ]
}

fn plugin_sections(index: usize, plugin: &PluginInfo) -> Vec<Tree> {
   let subtitle = plugin
      .error
      .as_deref()
      .map(|error| format!("Failed: {error}"));
   let status = match (&plugin.error, plugin.enabled) {
      (Some(_), _) => "failed",
      (None, true) => "running",
      (None, false) => "off",
   };
   let mut rows = Vec::new();
   let mut fields = Vec::new();
   for (at, setting) in plugin.settings.iter().enumerate() {
      let press = Press::Setting(index, at);
      let description = setting.description.as_deref();
      let field = |value: &str, entry| {
         let input = text_field(
            Field::Setting(index, at),
            None,
            description.unwrap_or(""),
            value,
            entry,
         );
         section(&setting.title, vec![input])
      };
      match &setting.control {
         Control::Switch(on) => {
            rows.push(switch_row(
               None,
               &setting.title,
               description,
               *on,
               press,
               press,
            ));
         },
         Control::Button => {
            let mut row = button_row(&setting.title, ICON_PLUGIN, Some(press), false);
            if let Some(inner) = row.kids.first_mut() {
               inner.kids.remove(0);
            }
            rows.push(row);
         },
         Control::Text(current) => fields.push(field(current, InputKind::Text)),
         Control::Secret(current) => fields.push(field(current, InputKind::Secret)),
         Control::Number(number) => fields.push(field(&number.to_string(), InputKind::Number)),
         Control::Select { options, chosen } => {
            let rows = (0..)
               .zip(options)
               .map(|(option, choice)| {
                  select_row(
                     &choice.label,
                     choice.value == *chosen,
                     Press::Choose(index, at, option),
                  )
               })
               .collect();
            fields.push(section(&setting.title, rows));
         },
      }
   }
   let mut sections = vec![section("", vec![switch_row(
      Some(ICON_PLUGIN),
      "Enabled",
      subtitle.as_deref(),
      plugin.enabled,
      Press::Toggle(index),
      Press::Toggle(index),
   )])];
   if !rows.is_empty() {
      sections.push(section("Settings", rows));
   }
   sections.extend(fields);
   let mut actions = vec![
      button_row("Edit code", ICON_CONSOLE, Some(Press::EditCode(index)), true),
      button_row("Reload", ICON_RELOAD, Some(Press::Reload(index)), false),
   ];
   if plugin.installed {
      let mut uninstall = button_row(
         "Uninstall",
         ICON_UNINSTALL,
         Some(Press::Uninstall(index)),
         false,
      );
      // `root > row > [icon, label]`, both in the danger color.
      if let Some(row) = uninstall.kids.first_mut() {
         if let Some(icon) = row.kids.get_mut(0) {
            icon.set(Prop::TINT, Some(Value::I64(COLOR_ERROR)));
         }
         if let Some(label) = row.kids.get_mut(1) {
            label.set(Prop::COLOR, Some(Value::I64(COLOR_ERROR)));
         }
      }
      actions.push(uninstall);
   }
   sections.push(section("", actions));
   let mut info = vec![value_row("Name", &plugin.name)];
   for (label, value) in [
      ("Version", &plugin.header.version),
      ("Author", &plugin.header.author),
   ] {
      if let Some(value) = value {
         info.push(value_row(label, value));
      }
   }
   info.push(value_row("Status", status));
   sections.push(section("Info", info));
   sections
}
