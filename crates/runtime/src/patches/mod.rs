//! Plugin UI spliced into Phoenix's screens: buttons in the chat header and
//! the message box, lines under messages, rows in the message long-press
//! menu, profile sections and tabs, and slash commands in the `/` suggestions.
//!
//! - Rust's batches are mirrored (children, accessibility ids), and plugin
//!   nodes go next to stable accessibility ids, cloned from Phoenix's own
//!   neighbours.
//! - No plugin code runs on the threads that carry UI batches or view events.
//!   Rust's store thread can hold the engine while it emits a batch, and the
//!   main thread can be waited on with the UI lock held. Plugin callbacks run
//!   on a worker, and their results reach the screen as follow-up batches.

mod badges;
mod commands;
mod emoji;
mod mirror;
mod tabs;
pub mod targets;

pub use badges::Badge;

use std::{
   collections::{
      HashMap,
      HashSet,
      VecDeque,
   },
   fmt,
   sync::{
      LazyLock,
      Mutex,
      MutexGuard,
      OnceLock,
      PoisonError,
      atomic::{
         AtomicBool,
         AtomicU64,
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
   Node,
   Op,
   Prop,
   Props,
   ViewKind,
   decode,
   encode,
   set_prop,
};
use phoenix_wire::{
   DispatchEvent,
   Key,
   Value,
   decode_dispatch,
   encode_batch,
   encode_view_event,
};

use crate::{
   bridge::{
      self,
      Action,
      Sink,
   },
   patches::{
      mirror::{
         Mirror,
         prop,
      },
      targets::{
         Seen,
         Target,
         Templates,
      },
   },
   store,
};

/// Plugin node ids count up from here, clear of Rust's and the settings page's.
const FIRST_ID: i32 = 0x5800_0000;
const DISMISS: Key = Prop::named("dismiss").key();
const FORCE_VALUE: Prop = Prop::named("forceValue");
const ROW_LONG_PRESS: Key = Prop::named("rowLongPress").key();
const RESULTS_CAP: usize = 4096;
const PAGE: Prop = Prop::named("page");
const PAGE_EVENTS: [Key; 3] = [
   Prop::named("pageSelect").key(),
   Prop::named("pageSettle").key(),
   Prop::named("pageScroll").key(),
];
const LAYOUT: Key = Prop::named("layout").key();
const LONG_PRESS: Key = Prop::named("longPress").key();

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ContributionId(u64);

impl ContributionId {
   pub fn next() -> Self {
      static NEXT: AtomicU64 = AtomicU64::new(1);
      Self(NEXT.fetch_add(1, Ordering::Relaxed))
   }
}

impl fmt::Display for ContributionId {
   fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
      self.0.fmt(formatter)
   }
}

/// Something a plugin added to a [`Target`]. Its callbacks stay in the
/// engine.
pub struct Contribution {
   pub id:          ContributionId,
   pub plugin:      String,
   pub target:      Target,
   pub label:       String,
   pub icon:        Option<String>,
   pub danger:      bool,
   pub priority:    i32,
   /// Shown only where the plugin's `visible` says so.
   pub conditional: bool,
}

/// A slash command, as the `/` suggestions list it.
#[derive(Clone)]
pub struct Command {
   pub name:        String,
   pub description: String,
   pub icon:        Option<String>,
   pub plugin:      String,
}

/// What a contribution's nodes are about.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Place {
   Screen,
   Message(String),
   User(Option<String>),
}

#[derive(Clone)]
pub struct Rendered {
   pub text:  String,
   pub color: Option<i64>,
   /// Under a message's content: the color of its author and text (32-bit
   /// ARGB).
   pub tint:  Option<i64>,
}

/// A plugin's answer for one place: the text it renders, the badges it draws,
/// or whether its row shows.
#[derive(Clone)]
pub enum Outcome {
   Text(Option<Rendered>),
   Badges(Vec<Badge>),
   Shown(bool),
   /// A profile tab's page.
   Page(Vec<Block>),
}

/// A piece of a profile tab's page. `handler` indexes the callbacks the
/// engine kept for the page.
#[derive(Clone)]
pub enum Block {
   Text {
      title: Option<String>,
      text:  String,
      color: Option<i64>,
   },
   Entry {
      avatar:   Option<String>,
      title:    String,
      subtitle: Option<String>,
      text:     String,
      /// Small images after the title, like badges.
      badges:   Vec<String>,
      /// Images under the text.
      images:   Vec<String>,
      /// A row under the text. Actions without a handler are plain text.
      actions:  Vec<BlockAction>,
      /// A column at the entry's end, like votes.
      side:     Vec<BlockAction>,
      /// Runs on a long press of the entry.
      hold:     Option<u32>,
   },
   /// Buttons side by side.
   Buttons(Vec<BlockAction>),
   Input {
      placeholder: String,
      handler:     u32,
      /// A send button, which submits like the keyboard's send key.
      send:        bool,
   },
   Button {
      label:   String,
      danger:  bool,
      handler: u32,
   },
}

#[derive(Clone)]
pub struct BlockAction {
   pub label:   String,
   pub danger:  bool,
   /// 32-bit ARGB, over `danger`.
   pub color:   Option<i64>,
   pub handler: Option<u32>,
}

/// What plugins made of a message about to be sent.
pub enum Sent {
   Cancel,
   Text(String),
   Unchanged,
}

/// What plugin callbacks receive as `ctx`.
#[derive(Default)]
pub struct UiContext {
   pub channel_id: Option<String>,
   pub message_id: Option<String>,
   pub user_id:    Option<String>,
   /// The message as Phoenix cached it, JSON.
   pub message:    Option<String>,
}

#[derive(Clone)]
enum Handler {
   Press {
      contribution: ContributionId,
      place:        Place,
      dismiss:      bool,
   },
   /// A command row in the `/` suggestions.
   Pick(String),
   /// A plugin's profile tab.
   Tab { contribution: ContributionId },
   /// A press in a profile tab's page.
   Block {
      contribution: ContributionId,
      index:        u32,
   },
   /// A text box in a profile tab's page.
   Input {
      contribution: ContributionId,
      index:        u32,
   },
   /// A profile tab page's content, which reports its height.
   Page { contribution: ContributionId },
   /// The send button of a text box in a profile tab's page.
   Send {
      contribution: ContributionId,
      index:        u32,
   },
   /// A row of a plugin menu sheet, or with `index` -1 the sheet itself,
   /// which says when it was swiped away.
   Choice { menu: u32, index: i32 },
   /// One of a plugin's badges in the badge directory.
   Detail { badge: Badge },
   /// An entry in a profile tab's page that takes long presses.
   Hold {
      contribution: ContributionId,
      index:        u32,
   },
}

/// A node to create, with the handler its subtree answers presses with.
pub struct Draft {
   kind:  ViewKind,
   props: Props,
   kids:  Vec<Self>,
   press: Option<Handler>,
}

impl Draft {
   fn from_node(node: &Node) -> Self {
      Self {
         kind:  node.kind.clone(),
         props: node.props.clone(),
         kids:  node.kids.iter().map(Self::from_node).collect(),
         press: None,
      }
   }

   fn set(&mut self, key: Prop, value: Option<Value>) {
      set_prop(&mut self.props, key, value);
   }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SlotKind {
   Target(Target),
   Suggestions,
   /// A plugin menu sheet.
   Menu(u32),
}

/// A menu sheet row a plugin asked for.
pub struct MenuItem {
   pub label:  String,
   pub icon:   Option<String>,
   pub danger: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Before {
   Start,
   Node(i32),
   End,
}

/// Plugin nodes living under one of Phoenix's parents.
struct Slot {
   kind:   SlotKind,
   /// The Phoenix node the slot was opened for.
   anchor: i32,
   parent: i32,
   before: Before,
   place:  Place,
   roots:  Vec<i32>,
   all:    Vec<i32>,
}

enum Job {
   Refresh(Target),
   Evaluate(ContributionId, Place),
   Evaluated(ContributionId, Place, Outcome),
   Press(ContributionId, Place),
   Suggest,
   Pick {
      input: i32,
      name:  String,
   },
   Submit {
      input: i32,
      text:  String,
      frame: Vec<u8>,
   },
   /// Open a plugin's profile tab on `pager`.
   Select {
      pager:        i32,
      contribution: ContributionId,
   },
   /// Open Phoenix's tab `index` on `pager`.
   Leave {
      pager: i32,
      index: i32,
   },
   Block {
      contribution: ContributionId,
      place:        Place,
      index:        u32,
      text:         Option<String>,
      /// The text box to empty.
      input:        Option<i32>,
   },
   PageHeight {
      pager:        i32,
      contribution: ContributionId,
      height:       f64,
   },
   /// Submit the text box `index` on `contribution`'s page.
   Send {
      contribution: ContributionId,
      place:        Place,
      index:        u32,
   },
   /// Show a menu sheet; the chosen row's index, or -1, goes to `answer`,
   /// and -2 when no sheet can be drawn.
   Menu {
      title:  Option<String>,
      items:  Vec<MenuItem>,
      answer: Sender<i32>,
   },
   /// Close menu `menu`, answering `index`.
   CloseMenu { menu: u32, index: i32 },
   /// Show one badge in the badge directory's detail sheet.
   Detail { badge: Badge },
   /// A press on custom emoji `id` in the picker, held for the `onEmojiPick`
   /// handlers.
   Emoji {
      input: i32,
      id:    String,
      text:  String,
      frame: Vec<u8>,
   },
}

#[derive(Default)]
struct Patches {
   mirror:        Mirror,
   templates:     Templates,
   seen:          Seen,
   contributions: Vec<Contribution>,
   commands:      Vec<Command>,
   interceptors:  usize,
   pickers:       usize,
   slots:         Vec<Slot>,
   anchors:       HashMap<Target, HashSet<i32>>,
   /// Badge directory titles still waiting for their grid, with the retries spent.
   badge_retries: HashMap<i32, u8>,
   handlers:      HashMap<i32, Handler>,
   results:       HashMap<(ContributionId, Place), Outcome>,
   pending:       HashSet<(ContributionId, Place)>,
   next_id:       i32,
   /// The channel last seen open.
   channel:       Option<String>,
   suggestions:   commands::Suggestions,
   /// Sends plugins already saw, let through once when Rust gets them back.
   resent:        Vec<Vec<u8>>,
   /// Message text color plugins asked for, by message id.
   tints:         HashMap<String, i64>,
   /// Phoenix's author and content text nodes: their message and props as
   /// Phoenix set them.
   texts:         HashMap<i32, (String, Props)>,
   /// The plugin tab open on each profile pager.
   shown:         HashMap<i32, ContributionId>,
   /// The height Rust last gave each pager.
   rust_heights:  HashMap<i32, Value>,
   /// The height of each plugin page, by pager.
   page_heights:  HashMap<(i32, ContributionId), f64>,
   /// What is typed in the text boxes of plugin pages, so a redraw keeps it.
   typed:         HashMap<(ContributionId, Place, u32), String>,
   /// A plugin tab's text and bar, by the tab.
   tab_parts:     HashMap<i32, (i32, i32)>,
   /// Open menu sheets and who waits for them.
   menus:         HashMap<u32, Sender<i32>>,
   /// Pagers whose own tab labels we color, since a plugin tab was opened:
   /// their animation stops at Phoenix's last tab and would light it up.
   static_tabs:   HashSet<i32>,
}

static PATCHES: LazyLock<Mutex<Patches>> = LazyLock::new(|| {
   Mutex::new(Patches {
      next_id: FIRST_ID,
      ..Patches::default()
   })
});
/// Set once a plugin adds anything, so builds without UI plugins never
/// decode a batch.
static ACTIVE: AtomicBool = AtomicBool::new(false);
static WORKER: OnceLock<Sender<Job>> = OnceLock::new();
/// Your messages' text as Discord has it, by the text plugin hooks turned it
/// into. Phoenix fills the edit box from the hooked text, and saving it would
/// post what the plugins added.
static ORIGINALS: Mutex<VecDeque<(String, String)>> = Mutex::new(VecDeque::new());
static GUARDING: AtomicBool = AtomicBool::new(false);
const ORIGINALS_CAP: usize = 256;

/// Remembers that a plugin hook turned your message `original` into `shown`.
pub fn remember_original(shown: String, original: String) {
   let mut originals = ORIGINALS.lock().unwrap_or_else(PoisonError::into_inner);
   originals.retain(|(known, _)| *known != shown);
   if originals.len() == ORIGINALS_CAP {
      originals.pop_front();
   }
   originals.push_back((shown, original));
   drop(originals);
   GUARDING.store(true, Ordering::Release);
}

/// Swaps hooked text Rust puts in an input back to what Discord has. Returns
/// whether it swapped any.
fn restore_originals(ops: &mut [Op]) -> bool {
   let originals = ORIGINALS.lock().unwrap_or_else(PoisonError::into_inner);
   let original = |text: &str| {
      originals
         .iter()
         .find(|(shown, _)| shown == text)
         .map(|(_, original)| original.clone())
   };
   let mut restored = false;
   for op in ops {
      let (Op::Update { props, .. } | Op::Create { props, .. }) = op else {
         continue;
      };
      for (key, value) in props.iter_mut() {
         let text = match value {
            Value::Str(text) if *key == Prop::VALUE => text,
            Value::Obj(fields) if *key == FORCE_VALUE => {
               let Some((_, Value::Str(text))) = fields
                  .iter_mut()
                  .find(|(field, _)| *field == Prop::TEXT.key())
               else {
                  continue;
               };
               text
            },
            _ => continue,
         };
         if let Some(original) = original(text) {
            *text = original;
            restored = true;
         }
      }
   }
   restored
}

fn patches() -> MutexGuard<'static, Patches> {
   PATCHES.lock().unwrap_or_else(PoisonError::into_inner)
}

fn queue(job: Job) {
   if let Some(worker) = WORKER.get()
      && worker.send(job).is_err()
   {
      log::warn!("patches: the worker is gone");
   }
}

struct PatchSink;

impl Sink for PatchSink {
   fn ops(&self, batch: &[u8]) -> Action {
      let active = ACTIVE.load(Ordering::Acquire);
      if !active && !GUARDING.load(Ordering::Acquire) {
         return Action::Pass;
      }
      let mut ops = match decode(batch) {
         Ok(ops) => ops,
         Err(err) => {
            log::warn!("patches: undecodable batch: {err}");
            return Action::Pass;
         },
      };
      let restored = restore_originals(&mut ops);
      let rewritten = if active {
         patches().rewrite(ops, restored)
      } else {
         restored.then_some(ops)
      };
      rewritten.map_or(Action::Pass, |ops| Action::Replace(encode(&ops)))
   }

   fn dispatch(&self, frame: &[u8]) -> Action {
      if !ACTIVE.load(Ordering::Acquire) {
         return Action::Pass;
      }
      patches().filter(frame)
   }
}

/// Hooks the UI bridge and starts the worker that runs plugin callbacks.
pub fn start() {
   let (sender, receiver) = mpsc::channel::<Job>();
   if WORKER.set(sender).is_err() {
      log::warn!("patches: already started");
      return;
   }
   bridge::install(Box::new(PatchSink));
   // The chat header and message box are built once, before the first
   // gateway event would attach the bridge.
   bridge::attach();
   thread::spawn(move || {
      for job in receiver {
         let ops = run(job);
         if !ops.is_empty() && !bridge::apply_ops(&encode(&ops)) {
            log::warn!("patches: the UI update did not apply");
         }
      }
   });
}

/// Runs `job`'s plugin calls, then returns the ops it leaves for the screen.
/// The engine and the patch state are never held together.
fn run(job: Job) -> Vec<Op> {
   match job {
      Job::Refresh(target) => {
         let mut state = patches();
         Target::ALL
            .into_iter()
            .filter(|drawn| drawn.drawn_by() == target)
            .flat_map(|drawn| state.refresh(drawn))
            .collect()
      },
      Job::Select {
         pager,
         contribution,
      } => patches().select_tab(pager, contribution),
      Job::Leave { pager, index } => patches().leave_tab(pager, Some(index)),
      Job::PageHeight {
         pager,
         contribution,
         height,
      } => {
         let mut state = patches();
         state.page_heights.insert((pager, contribution), height);
         if state.shown.get(&pager) == Some(&contribution) {
            vec![Op::Update {
               id:    pager,
               props: vec![(Prop::HEIGHT, Value::F64(height))],
            }]
         } else {
            Vec::new()
         }
      },
      Job::Send {
         contribution,
         place,
         index,
      } => {
         let mut state = patches();
         let input = state.input_node(contribution, &place, index);
         let text = state.typed.remove(&(contribution, place.clone(), index));
         drop(state);
         if let Some(engine) = crate::engine() {
            engine.ui_block(contribution, &place, index, Some(text.unwrap_or_default()));
         }
         input.map(|input| vec![commands::force_text(input, "")]).unwrap_or_default()
      },
      Job::Menu {
         title,
         items,
         answer,
      } => patches().open_menu(title.as_deref(), items, answer),
      Job::CloseMenu { menu, index } => patches().close_menu(menu, index),
      Job::Detail { badge } => patches().open_badge_detail(&badge),
      Job::Block {
         contribution,
         place,
         index,
         text,
         input,
      } => {
         if let Some(engine) = crate::engine() {
            engine.ui_block(contribution, &place, index, text);
         }
         input.map(|input| vec![commands::force_text(input, "")]).unwrap_or_default()
      },
      Job::Evaluate(contribution, place) => {
         let context = UiContext {
            channel_id: patches().channel.clone(),
            message_id: match &place {
               Place::Message(id) => Some(id.clone()),
               _ => None,
            },
            user_id:    match &place {
               Place::User(id) => id.clone(),
               _ => None,
            },
            message:    None,
         };
         let outcome =
            crate::engine().and_then(|engine| engine.ui_evaluate(contribution, &place, &context));
         outcome.map_or_else(Vec::new, |outcome| {
            patches().evaluated(contribution, place, outcome)
         })
      },
      Job::Evaluated(contribution, place, outcome) => {
         patches().evaluated(contribution, place, outcome)
      },
      Job::Press(contribution, place) => {
         let context = press_context(&place);
         if let Some(engine) = crate::engine() {
            engine.ui_press(contribution, &context);
         }
         Vec::new()
      },
      Job::Suggest => {
         let mut out = Vec::new();
         patches().suggest(&mut out);
         out
      },
      Job::Pick { input, name } => {
         let mut state = patches();
         let mut out = vec![commands::force_text(input, &format!("/{name} "))];
         state.mirror.set_composer_text(input, format!("/{name} "));
         state.suggest(&mut out);
         out
      },
      Job::Submit { input, text, frame } => commands::submit(input, &text, &frame),
      Job::Emoji {
         input,
         id,
         text,
         frame,
      } => emoji::pick(input, &id, text, frame),
   }
}

/// `ctx` for a press: the open channel, and the message when there is one.
/// It reads Phoenix's cache, so it runs on the worker.
fn press_context(place: &Place) -> UiContext {
   let database = crate::STATE
      .get()
      .and_then(|state| store::database(&state.files_dir()).ok());
   let rows = |sql: &str, args: &[String]| {
      let database = database.as_ref()?;
      store::query(database, sql, args)
         .inspect_err(|err| log::warn!("patches: {sql}: {err}"))
         .ok()
   };
   // Phoenix keeps its JSON values as BLOBs, which come back as byte arrays.
   let value = |rows: &[serde_json::Map<String, serde_json::Value>], key: &str| {
      match rows.first()?.get(key)? {
         serde_json::Value::String(text) => Some(text.clone()),
         serde_json::Value::Array(bytes) => {
            let bytes = bytes
               .iter()
               .map(|byte| u8::try_from(byte.as_u64()?).ok())
               .collect::<Option<Vec<u8>>>()?;
            String::from_utf8(bytes).ok()
         },
         _ => None,
      }
   };

   let message = |id: &str| {
      let found = rows("SELECT k, v FROM messages WHERE k LIKE ? LIMIT 1", &[
         format!("%:{id:0>20}"),
      ])
      .unwrap_or_default();
      let channel = value(&found, "k")
         .and_then(|key| Some(key.split(':').next()?.trim_start_matches('0').to_owned()));
      (channel, value(&found, "v"))
   };

   let mut context = UiContext::default();
   if let Place::Message(id) = place {
      (context.channel_id, context.message) = message(id);
      context.message_id = Some(id.clone());
   }
   if let Place::User(id) = place {
      context.user_id.clone_from(id);
   }
   if context.channel_id.is_none() {
      let selected =
         rows("SELECT v FROM meta WHERE k = ?", &["selected".to_owned()]).unwrap_or_default();
      context.channel_id = value(&selected, "v")
         .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
         .and_then(|selected| {
            match selected.get("channel_id")? {
               serde_json::Value::String(id) => Some(id.clone()),
               serde_json::Value::Number(id) => Some(id.to_string()),
               _ => None,
            }
         });
   }
   if context.channel_id.is_none() {
      let newest = patches().newest_message();
      context.channel_id = newest.and_then(|id| message(&id).0);
   }
   if context.channel_id.is_some() {
      patches().channel.clone_from(&context.channel_id);
   }
   context
}

/// Adds `contribution` and draws it wherever its target is on screen.
pub fn add(contribution: Contribution) {
   ACTIVE.store(true, Ordering::Release);
   let target = contribution.target;
   let mut state = patches();
   state.contributions.push(contribution);
   state
      .contributions
      .sort_by_key(|contribution| std::cmp::Reverse(contribution.priority));
   drop(state);
   queue(Job::Refresh(target));
}

pub fn remove(id: ContributionId) {
   let mut state = patches();
   let Some(at) = state
      .contributions
      .iter()
      .position(|contribution| contribution.id == id)
   else {
      return;
   };
   let target = state.contributions.remove(at).target;
   state
      .results
      .retain(|(contribution, _), _| *contribution != id);
   drop(state);
   queue(Job::Refresh(target));
}

/// Draws `id` again, asking its plugin anew: everywhere, or only at `place`,
/// where the old result stays up until the new one is in.
pub fn update(id: ContributionId, place: Option<Place>) {
   let mut state = patches();
   if let Some(place) = place {
      if state.pending.insert((id, place.clone())) {
         queue(Job::Evaluate(id, place));
      }
      return;
   }
   let Some(target) = state
      .contributions
      .iter()
      .find(|contribution| contribution.id == id)
      .map(|contribution| contribution.target)
   else {
      return;
   };
   state
      .results
      .retain(|(contribution, _), _| *contribution != id);
   drop(state);
   queue(Job::Refresh(target));
}

/// Shows a menu sheet like the message long-press one and waits for the
/// choice: the row's index, -1 when it was swiped away, or `None` when no
/// such sheet can be drawn yet.
pub fn menu(title: Option<String>, items: Vec<MenuItem>) -> Option<i32> {
   let (answer, receiver) = mpsc::channel();
   queue(Job::Menu { title, items, answer });
   match receiver.recv() {
      Ok(-2) | Err(_) => None,
      Ok(index) => Some(index),
   }
}

/// A plugin settled a `render` or `visible` it returned as a promise.
pub fn evaluated(contribution: ContributionId, place: Place, outcome: Outcome) {
   queue(Job::Evaluated(contribution, place, outcome));
}

pub fn add_command(command: Command) {
   ACTIVE.store(true, Ordering::Release);
   let mut state = patches();
   state
      .commands
      .retain(|existing| existing.name != command.name);
   state.commands.push(command);
   drop(state);
   queue(Job::Suggest);
}

pub fn remove_command(name: &str) {
   patches().commands.retain(|command| command.name != name);
   queue(Job::Suggest);
}

pub fn commands() -> Vec<Command> {
   patches().commands.clone()
}

/// How many `onSend` interceptors plugins hold, so sends are taken only
/// while one exists.
pub fn set_interceptors(count: usize) {
   if count > 0 {
      ACTIVE.store(true, Ordering::Release);
   }
   patches().interceptors = count;
}

/// How many `onEmojiPick` handlers plugins hold, so picker presses are taken,
/// and Nitro-locked emojis offered, only while one exists.
pub fn set_emoji_pickers(count: usize) {
   if count > 0 {
      ACTIVE.store(true, Ordering::Release);
   }
   patches().pickers = count;
   emoji::unlock(count > 0);
}

/// Drops everything `plugin` added.
pub fn forget(plugin: &str) {
   let mut state = patches();
   let targets = state
      .contributions
      .iter()
      .filter(|contribution| contribution.plugin == plugin)
      .map(|contribution| contribution.target)
      .collect::<HashSet<_>>();
   let gone = state
      .contributions
      .iter()
      .filter(|contribution| contribution.plugin == plugin)
      .map(|contribution| contribution.id)
      .collect::<HashSet<_>>();
   state
      .contributions
      .retain(|contribution| contribution.plugin != plugin);
   state
      .results
      .retain(|(contribution, _), _| !gone.contains(contribution));
   let had_commands = state
      .commands
      .iter()
      .any(|command| command.plugin == plugin);
   state.commands.retain(|command| command.plugin != plugin);
   drop(state);
   for target in targets {
      queue(Job::Refresh(target));
   }
   if had_commands {
      queue(Job::Suggest);
   }
}

impl Patches {
   /// Mirrors `ops` and splices plugin nodes in. `None` leaves the batch as
   /// it was.
   fn rewrite(&mut self, ops: Vec<Op>, restored: bool) -> Option<Vec<Op>> {
      let mut mine = Vec::new();
      let mut touched = HashSet::new();
      let mut extra_deletes;
      {
         let (created, deleted) = self.mirror.apply(&ops);
         self.templates.learn_badge_tile(&self.mirror, &created);
         extra_deletes = self.drop_deleted(&deleted);
         self.track_badge_user(&ops, &created);
         for id in &deleted {
            self.texts.remove(id);
         }
         for (id, (kind, props)) in &created {
            if **kind == ViewKind::Text
               && let Some(message) = self.text_message(*id)
            {
               self.texts.insert(*id, (message, (*props).clone()));
            }
         }
         for op in &ops {
            if let Op::Update { id, props } = op
               && let Some(height) = prop(props, Prop::HEIGHT)
               && self.anchors.get(&Target::Tab).is_some_and(|pagers| pagers.contains(id))
            {
               self.rust_heights.insert(*id, height.clone());
            }
            if let Op::Update { id, props } = op
               && let Some((_, stored)) = self.texts.get_mut(id)
            {
               for (key, value) in props {
                  set_prop(stored, *key, Some(value.clone()));
               }
            }
         }

         let mut named = created
            .keys()
            .copied()
            .filter(|id| self.mirror.accessibility_id(*id).is_some())
            .collect::<Vec<_>>();
         named.sort_unstable();
         for id in &named {
            let aid = self
               .mirror
               .accessibility_id(*id)
               .unwrap_or_default()
               .to_owned();
            self.templates.learn(&self.mirror, *id, &aid, &created);
            if aid == "suggestions" {
               self.suggestions.scroll = Some(*id);
            }
            if aid == "profile-tab-pages" {
               self.seen.profile_user = profile_user(&ops);
            }
         }
         if self.suggestions.scroll.is_some() {
            self.learn_suggestion_row(&created);
         }
         for id in named {
            let aid = self
               .mirror
               .accessibility_id(id)
               .unwrap_or_default()
               .to_owned();
            for target in Target::ALL {
               if !target.matches(&aid) {
                  continue;
               }
               self.anchors.entry(target).or_default().insert(id);
               if target == Target::Tab
                  && let Some((_, props)) = created.get(&id)
                  && let Some(height) = prop(props, Prop::HEIGHT)
               {
                  self.rust_heights.insert(id, height.clone());
               }
               if let Some(parent) = self.place(target, id, &mut mine) {
                  touched.insert(parent);
               }
            }
         }
         self.anchor_badges(&ops, &mut mine, &mut touched);
      }

      let composer_slash = self
         .mirror
         .composer()
         .is_some_and(|(_, text)| text.starts_with('/'));
      let mut extra = Vec::new();
      if !self.commands.is_empty()
         && self.suggestions.scroll.is_some()
         && (self.has_suggestions() || composer_slash)
      {
         self.suggest(&mut extra);
      }

      let slot_parents = self
         .slots
         .iter()
         .map(|slot| slot.parent)
         .collect::<HashSet<_>>();
      let rewrites_children = ops.iter().any(|op| {
         matches!(op, Op::Children { id, .. } | Op::ChildEdits { id, .. } if slot_parents.contains(id))
      });
      let tabbed = ops
         .iter()
         .any(|op| matches!(op, Op::Update { id, .. }
            if self.shown.contains_key(id) || self.static_tabs.contains(id)));
      let recolors = !self.tints.is_empty()
         && ops.iter().any(|op| {
            matches!(op, Op::Create { id, .. } | Op::Update { id, .. } if self.tint_of(*id).is_some())
         });
      if touched.is_empty()
         && extra_deletes.is_empty()
         && extra.is_empty()
         && !rewrites_children
         && !restored
         && !recolors
         && !tabbed
      {
         return None;
      }

      let mut out = Vec::new();
      let mut left = Vec::new();
      let mut recolor = Vec::new();
      for op in ops {
         match op {
            Op::Update { id, mut props }
               if self.shown.contains_key(&id) || self.static_tabs.contains(&id) =>
            {
               if let Some(page) = prop(&props, PAGE) {
                  // Rust opened one of its tabs.
                  if self.shown.contains_key(&id) {
                     left.push(id);
                  }
                  let index = page
                     .field(Prop::INDEX.key())
                     .and_then(Value::as_i32)
                     .and_then(|index| usize::try_from(index).ok());
                  recolor.push((id, index));
               } else if self.shown.contains_key(&id)
                  && prop(&props, Prop::HEIGHT).is_some()
                  && let Some(height) = self
                     .shown
                     .get(&id)
                     .and_then(|shown| self.page_heights.get(&(id, *shown)))
               {
                  // Rust sized the pager to its own page, under ours.
                  set_prop(&mut props, Prop::HEIGHT, Some(Value::F64(*height)));
               }
               out.push(Op::Update { id, props });
            },
            Op::Children { id, .. } | Op::ChildEdits { id, .. } if slot_parents.contains(&id) => {
               // Plugin nodes are created after the batch, so the merged list
               // has to be written after them too.
               touched.insert(id);
            },
            Op::Delete { mut ids } if !extra_deletes.is_empty() => {
               ids.append(&mut extra_deletes);
               out.push(Op::Delete { ids });
            },
            Op::Create { id, kind, props } if let Some(color) = self.tint_of(id) => {
               out.push(Op::Create {
                  id,
                  kind,
                  props: recolored(&props, color, true),
               });
            },
            Op::Update { id, props } if let Some(color) = self.tint_of(id) => {
               out.push(Op::Update {
                  id,
                  props: recolored(&props, color, false),
               });
            },
            op => out.push(op),
         }
      }
      // Plugin nodes go after the batch that creates the rows they sit in,
      // or the host drops them as orphans.
      out.extend(mine);
      for parent in touched {
         out.push(Op::Children {
            id:  parent,
            ids: self.merged(parent),
         });
      }
      out.extend(extra);
      for pager in left {
         out.extend(self.leave_tab(pager, None));
      }
      for (pager, index) in recolor {
         out.extend(self.rust_label_colors(pager, index));
      }
      Some(out)
   }

   /// Forgets the slots and anchors under `deleted` nodes and returns the
   /// plugin nodes that go with them.
   fn drop_deleted(&mut self, deleted: &[i32]) -> Vec<i32> {
      let mut orphans = Vec::new();
      for id in deleted {
         let (gone, kept) = std::mem::take(&mut self.slots)
            .into_iter()
            .partition::<Vec<_>, _>(|slot| slot.parent == *id);
         self.slots = kept;
         for slot in gone {
            if let SlotKind::Menu(menu) = slot.kind
               && let Some(answer) = self.menus.remove(&menu)
            {
               let _ = answer.send(-1);
            }
            self.forget_slot(&slot);
            orphans.extend(slot.all);
         }
         for anchors in self.anchors.values_mut() {
            anchors.remove(id);
         }
         self.badge_retries.remove(id);
         self.shown.remove(id);
         self.static_tabs.remove(id);
         self.rust_heights.remove(id);
         self.page_heights.retain(|(pager, _), _| pager != id);
         self.tab_parts.remove(id);
         if self.suggestions.scroll == Some(*id) {
            orphans.extend(self.drop_suggestions());
         }
      }
      orphans
   }

   /// Takes presses on plugin nodes, long presses, and sends out of a
   /// Java-to-Rust dispatch.
   fn filter(&mut self, frame: &[u8]) -> Action {
      let Ok(items) = decode_dispatch(frame) else {
         return Action::Pass;
      };
      let mut changed = false;
      let mut frames = Vec::new();
      for item in &items {
         let raw = frame[item.offset..item.offset + item.len].to_vec();
         if let Some(at) = self.resent.iter().position(|sent| *sent == raw) {
            self.resent.swap_remove(at);
            frames.push(raw);
            continue;
         }
         let DispatchEvent::View {
            node,
            event,
            payload,
         } = &item.event
         else {
            frames.push(raw);
            continue;
         };
         if PAGE_EVENTS.contains(event) && self.beyond_rust(*node, payload) {
            // About a plugin page, which Rust does not know.
            changed = true;
            continue;
         }
         if *event == Prop::PRESS.key()
            && let Some((pager, index)) = self.rust_tab(*node)
            && self.shown.contains_key(&pager)
         {
            queue(Job::Leave { pager, index });
         }
         if *event == ROW_LONG_PRESS
            && let Some(index) = payload.field(Prop::INDEX.key()).and_then(Value::as_i32)
         {
            self.seen.long_pressed = self.row_message(*node, index);
         }
         if *event == Prop::CHANGE_TEXT.key() && self.mirror.is_composer(*node) {
            let text = payload
               .field(Prop::TEXT.key())
               .and_then(Value::as_str)
               .unwrap_or_default()
               .to_owned();
            self.mirror.set_composer_text(*node, text);
            if !self.commands.is_empty() {
               queue(Job::Suggest);
            }
         }
         if *node >= FIRST_ID
            && let Some(handler) = self.handlers.get(node).cloned()
         {
            changed = true;
            if *event == Prop::PRESS.key() {
               self.pressed(*node, handler, &mut frames);
            } else {
               self.page_event(*node, &handler, *event, payload);
            }
            continue;
         }
         if self.pickers > 0
            && *event == Prop::PRESS.key()
            && let Some(id) = self
               .mirror
               .accessibility_id(*node)
               .and_then(|aid| aid.strip_prefix("expression.emoji:custom:"))
            && self.in_message_picker(*node)
            && let Some((input, text)) = self.mirror.composer()
         {
            queue(Job::Emoji {
               input,
               id: id.to_owned(),
               text: text.to_owned(),
               frame: raw,
            });
            changed = true;
            continue;
         }
         if let Some(input) = self.sending(*node, *event)
            && let Some((_, text)) = self.mirror.composer()
         {
            queue(Job::Submit {
               input,
               text: text.to_owned(),
               frame: raw,
            });
            changed = true;
            continue;
         }
         frames.push(raw);
      }
      if !changed {
         return Action::Pass;
      }
      match frames.len() {
         0 => Action::Drop,
         1 => Action::Replace(frames.remove(0)),
         _ => Action::Replace(encode_batch(&frames)),
      }
   }

   fn pressed(&self, node: i32, handler: Handler, frames: &mut Vec<Vec<u8>>) {
      match handler {
         Handler::Press {
            contribution,
            place,
            dismiss,
         } => {
            queue(Job::Press(contribution, place));
            let slot = self.slots.iter().find(|slot| slot.all.contains(&node));
            if dismiss && let Some(sheet) = slot.and_then(|slot| self.mirror.sheet_of(slot.parent))
            {
               frames.push(encode_view_event(sheet, DISMISS, &Value::Obj(Vec::new())));
            }
         },
         Handler::Tab { contribution } => {
            if let Some(slot) = self.slots.iter().find(|slot| slot.all.contains(&node)) {
               queue(Job::Select {
                  pager: slot.anchor,
                  contribution,
               });
            }
         },
         Handler::Block {
            contribution,
            index,
         } => {
            if let Some(slot) = self.slots.iter().find(|slot| slot.all.contains(&node)) {
               queue(Job::Block {
                  contribution,
                  place: slot.place.clone(),
                  index,
                  text: None,
                  input: None,
               });
            }
         },
         Handler::Send {
            contribution,
            index,
         } => {
            if let Some(slot) = self.slots.iter().find(|slot| slot.all.contains(&node)) {
               queue(Job::Send {
                  contribution,
                  place: slot.place.clone(),
                  index,
               });
            }
         },
         Handler::Choice { menu, index } => queue(Job::CloseMenu { menu, index }),
         Handler::Detail { badge } => queue(Job::Detail { badge }),
         Handler::Input { .. } | Handler::Page { .. } | Handler::Hold { .. } => {},
         Handler::Pick(name) => {
            if let Some((input, _)) = self.mirror.composer() {
               frames.push(commands::change_text(input, &format!("/{name} ")));
               queue(Job::Pick { input, name });
            }
         },
      }
   }

   /// Takes typing, sends and heights from a plugin page.
   fn page_event(&mut self, node: i32, handler: &Handler, event: Key, payload: &Value) {
      let slot = self.slots.iter().find(|slot| slot.all.contains(&node));
      match *handler {
         Handler::Input {
            contribution,
            index,
         } => {
            let Some(place) = slot.map(|slot| slot.place.clone()) else {
               return;
            };
            if event == Prop::CHANGE_TEXT.key() {
               let text = payload
                  .field(Prop::TEXT.key())
                  .and_then(Value::as_str)
                  .or_else(|| payload.as_str())
                  .unwrap_or_default();
               if self.typed.len() >= RESULTS_CAP {
                  self.typed.clear();
               }
               self.typed.insert((contribution, place, index), text.to_owned());
            } else if event == Prop::SUBMIT.key() {
               let text = self.typed.remove(&(contribution, place.clone(), index));
               queue(Job::Block {
                  contribution,
                  place,
                  index,
                  text: Some(text.unwrap_or_default()),
                  input: Some(node),
               });
            }
         },
         Handler::Hold {
            contribution,
            index,
         } if event == LONG_PRESS => {
            if let Some(slot) = slot {
               queue(Job::Block {
                  contribution,
                  place: slot.place.clone(),
                  index,
                  text: None,
                  input: None,
               });
            }
         },
         Handler::Choice { menu, .. } if event == DISMISS => {
            queue(Job::CloseMenu { menu, index: -1 });
         },
         Handler::Page { contribution } if event == LAYOUT => {
            if let (Some(slot), Some(height)) = (
               slot,
               payload.field(Prop::HEIGHT.key()).and_then(number),
            ) {
               queue(Job::PageHeight {
                  pager: slot.anchor,
                  contribution,
                  height,
               });
            }
         },
         _ => {},
      }
   }

   /// Shows a menu sheet over the top sheet, like the message long-press one.
   fn open_menu(&mut self, title: Option<&str>, items: Vec<MenuItem>, answer: Sender<i32>) -> Vec<Op> {
      static NEXT: AtomicU64 = AtomicU64::new(1);
      #[expect(clippy::cast_possible_truncation, reason = "menus are few")]
      let menu = NEXT.fetch_add(1, Ordering::Relaxed) as u32;
      let host = self
         .mirror
         .newest_sheet()
         .and_then(|sheet| self.mirror.parent(sheet))
         .and_then(|outer| self.mirror.parent(outer));
      let rows = items
         .into_iter()
         .enumerate()
         .map(|(index, item)| {
            targets::MenuRow {
               label:  item.label,
               icon:   item.icon,
               danger: item.danger,
               press:  Handler::Choice {
                  menu,
                  index: i32::try_from(index).unwrap_or_default(),
               },
               id:     None,
            }
         })
         .collect();
      let draft = self.templates.menu(title, rows, Handler::Choice { menu, index: -1 });
      let (Some(host), Some(draft)) = (host, draft) else {
         let _ = answer.send(-2);
         return Vec::new();
      };
      let mut out = Vec::new();
      let mut all = Vec::new();
      let root = self.emit(draft, None, &mut out, &mut all);
      self.slots.push(Slot {
         kind: SlotKind::Menu(menu),
         anchor: host,
         parent: host,
         before: Before::End,
         place: Place::Screen,
         roots: vec![root],
         all,
      });
      self.menus.insert(menu, answer);
      out.push(Op::Children {
         id:  host,
         ids: self.merged(host),
      });
      out
   }

   fn close_menu(&mut self, menu: u32, index: i32) -> Vec<Op> {
      if let Some(answer) = self.menus.remove(&menu) {
         let _ = answer.send(index);
      }
      let Some(at) = self.slots.iter().position(|slot| slot.kind == SlotKind::Menu(menu)) else {
         return Vec::new();
      };
      let slot = self.slots.remove(at);
      self.forget_ids(&slot.all);
      let mut out = Vec::new();
      if self.mirror.has_kids(slot.parent) {
         out.push(Op::Children {
            id:  slot.parent,
            ids: self.merged(slot.parent),
         });
      }
      out.push(Op::Delete { ids: slot.all });
      out
   }

   /// The node of text box `index` on `contribution`'s page at `place`.
   fn input_node(&self, contribution: ContributionId, place: &Place, index: u32) -> Option<i32> {
      self
         .slots
         .iter()
         .filter(|slot| slot.kind == SlotKind::Target(Target::TabPage) && slot.place == *place)
         .flat_map(|slot| slot.all.iter())
         .copied()
         .find(|node| {
            matches!(self.handlers.get(node), Some(Handler::Input { contribution: owner, index: at })
               if *owner == contribution && *at == index)
         })
   }

   /// Puts back what was typed into the text boxes of a page drawn anew.
   fn restore_typed(&self, draft: &mut Draft, place: &Place) {
      if let Some(Handler::Input {
         contribution,
         index,
      }) = &draft.press
         && let Some(text) = self.typed.get(&(*contribution, place.clone(), *index))
      {
         draft.set(Prop::VALUE, Some(Value::Str(text.clone())));
      }
      for kid in &mut draft.kids {
         self.restore_typed(kid, place);
      }
   }

   /// Whether a page event on `node` is about a page past Rust's.
   fn beyond_rust(&self, node: i32, payload: &Value) -> bool {
      if !self.anchors.get(&Target::Tab).is_some_and(|pagers| pagers.contains(&node)) {
         return false;
      }
      let Some(row) = self.mirror.kids(node).first() else {
         return false;
      };
      let last = self.mirror.kids(*row).len().saturating_sub(1);
      #[expect(clippy::cast_precision_loss, reason = "a handful of pages")]
      let last = last as f64;
      let page = payload.field(PAGE.key()).and_then(Value::as_i32).map(f64::from);
      let offset = payload.field(Prop::named("offset").key()).and_then(number);
      page.or(offset).is_some_and(|at| at > last + 0.001)
   }

   /// The pager and index of Rust's profile tab `node`.
   fn rust_tab(&self, node: i32) -> Option<(i32, i32)> {
      let row = self.mirror.parent(node)?;
      let slot = self
         .slots
         .iter()
         .find(|slot| slot.parent == row && slot.kind == SlotKind::Target(Target::Tab))?;
      let index = self.mirror.kids(row).iter().position(|tab| *tab == node)?;
      Some((slot.anchor, i32::try_from(index).ok()?))
   }

   /// Opens `contribution`'s page on `pager`, after Rust's pages.
   fn select_tab(&mut self, pager: i32, contribution: ContributionId) -> Vec<Op> {
      let Some(at) = self
         .contributions
         .iter()
         .filter(|known| known.target == Target::Tab)
         .position(|known| known.id == contribution)
      else {
         return Vec::new();
      };
      let Some(&row) = self.mirror.kids(pager).first() else {
         return Vec::new();
      };
      let page = self.mirror.kids(row).len() + at;
      let mut out = vec![page_op(pager, page)];
      if let Some(height) = self.page_heights.get(&(pager, contribution)) {
         out.push(Op::Update {
            id:    pager,
            props: vec![(Prop::HEIGHT, Value::F64(*height))],
         });
      }
      if let Some((tabs, indicator)) = targets::tab_row(&self.mirror, pager) {
         out.push(Op::Update {
            id:    indicator,
            props: vec![(HIDDEN, Value::Bool(true))],
         });
         if let Some(strip) = self.mirror.parent(tabs) {
            let index = self.mirror.kids(tabs).len() - 1 + at;
            out.push(Op::ScrollTo {
               id:    strip,
               value: Value::Obj(vec![
                  (Prop::INDEX.key(), Value::I32(i32::try_from(index).unwrap_or_default())),
                  (Prop::named("align").key(), Value::Str("reveal".to_owned())),
                  (Prop::named("offset").key(), Value::F64(16.0)),
                  (Prop::named("animated").key(), Value::Bool(true)),
               ]),
            });
         }
      }
      self.shown.insert(pager, contribution);
      self.static_tabs.insert(pager);
      out.extend(self.rust_label_colors(pager, None));
      out.extend(self.tab_looks(pager));
      out
   }

   /// Closes the plugin page open on `pager`, opening Rust's page `index`
   /// unless Rust does that itself.
   fn leave_tab(&mut self, pager: i32, index: Option<i32>) -> Vec<Op> {
      if self.shown.remove(&pager).is_none() {
         return Vec::new();
      }
      let mut out = Vec::new();
      if let Some(index) = index {
         out.push(page_op(pager, usize::try_from(index).unwrap_or_default()));
         out.extend(self.rust_label_colors(pager, usize::try_from(index).ok()));
      }
      if let Some(height) = self.rust_heights.get(&pager) {
         out.push(Op::Update {
            id:    pager,
            props: vec![(Prop::HEIGHT, height.clone())],
         });
      }
      if let Some((_, indicator)) = targets::tab_row(&self.mirror, pager) {
         out.push(Op::Update {
            id:    indicator,
            props: vec![(HIDDEN, Value::Null)],
         });
      }
      out.extend(self.tab_looks(pager));
      out
   }

   /// Colors Phoenix's own tab labels on `pager`, `selected` lit, with their
   /// animation off.
   fn rust_label_colors(&self, pager: i32, selected: Option<usize>) -> Vec<Op> {
      let (on, off) = self.templates.tab_colors();
      let Some((row, _)) = targets::tab_row(&self.mirror, pager) else {
         return Vec::new();
      };
      let tabs = self.mirror.kids(row);
      tabs[..tabs.len().saturating_sub(1)]
         .iter()
         .enumerate()
         .filter_map(|(index, tab)| {
            let inner = *self.mirror.kids(*tab).first()?;
            let text = *self.mirror.kids(inner).first()?;
            let color = if Some(index) == selected { on } else { off };
            Some(Op::Update {
               id:    text,
               props: vec![
                  (Prop::named("worklet"), Value::Null),
                  (Prop::COLOR, Value::I64(color)),
               ],
            })
         })
         .collect()
   }

   /// Colors the plugin tabs on `pager` and shows the open one's bar.
   fn tab_looks(&self, pager: i32) -> Vec<Op> {
      let (on, off) = self.templates.tab_colors();
      let shown = self.shown.get(&pager);
      let tabs = self
         .contributions
         .iter()
         .filter(|known| known.target == Target::Tab)
         .map(|known| known.id);
      let roots = self
         .slots
         .iter()
         .filter(|slot| slot.anchor == pager && slot.kind == SlotKind::Target(Target::Tab))
         .flat_map(|slot| slot.roots.iter().copied());
      let mut out = Vec::new();
      for (contribution, root) in tabs.zip(roots) {
         let Some((text, bar)) = self.tab_parts.get(&root) else {
            continue;
         };
         let open = shown == Some(&contribution);
         out.push(Op::Update {
            id:    *text,
            props: vec![(Prop::COLOR, Value::I64(if open { on } else { off }))],
         });
         out.push(Op::Update {
            id:    *bar,
            props: vec![(BACKGROUND, Value::I64(if open { on } else { 0 }))],
         });
         out.push(Op::Update {
            id:    root,
            props: vec![(Prop::named("accessibilitySelected"), Value::Bool(open))],
         });
      }
      out
   }

   /// The composer input when `event` on `node` sends a message and a plugin
   /// wants to see it first.
   fn sending(&self, node: i32, event: Key) -> Option<i32> {
      if self.commands.is_empty() && self.interceptors == 0 {
         return None;
      }
      let (input, _) = self.mirror.composer()?;
      let submit = event == Prop::SUBMIT.key() && self.mirror.is_composer(node);
      let send = event == Prop::PRESS.key() && self.mirror.accessibility_id(node) == Some("send");
      (submit || send).then_some(input)
   }

   /// The newest message on screen.
   fn newest_message(&self) -> Option<String> {
      self
         .anchors
         .get(&Target::Content)?
         .iter()
         .filter_map(|id| message_id(self.mirror.accessibility_id(*id)?))
         .max_by_key(|id| (id.len(), *id))
         .map(str::to_owned)
   }

   /// The message id in row `index` of list `node`.
   fn row_message(&self, node: i32, index: i32) -> Option<String> {
      let row = self.mirror.row(node, index)?;
      let hit = self
         .mirror
         .find_down(row, &|aid| message_id(aid).is_some(), 8)?;
      message_id(self.mirror.accessibility_id(hit)?).map(str::to_owned)
   }

   /// Opens `target`'s slot for the anchor `id` and draws into it. Returns
   /// the parent when it opened a new slot.
   fn place(&mut self, target: Target, id: i32, out: &mut Vec<Op>) -> Option<i32> {
      if !self
         .contributions
         .iter()
         .any(|contribution| contribution.target == target.drawn_by())
      {
         return None;
      }
      // The badge row's anchor is an image with no accessibility id.
      let aid = self.mirror.accessibility_id(id).unwrap_or_default().to_owned();
      let placement = target.place(&self.mirror, id, &aid, &self.seen);
      if target == Target::BadgeDirectory {
         self.retry_badge_placement(id, placement.is_some());
      }
      let placement = placement?;
      let before = placement.before.map_or(Before::End, Before::Node);
      if let Some(slot) = self
         .slots
         .iter_mut()
         .find(|slot| slot.parent == placement.parent && slot.kind == SlotKind::Target(target))
      {
         slot.before = before;
         return None;
      }
      self.slots.push(Slot {
         kind: SlotKind::Target(target),
         anchor: id,
         parent: placement.parent,
         before,
         place: placement.place,
         roots: Vec::new(),
         all: Vec::new(),
      });
      let index = self.slots.len() - 1;
      self.fill(index, out);
      Some(placement.parent)
   }

   /// Draws slot `index` anew into `out`. The caller deletes what it held.
   fn fill(&mut self, index: usize, out: &mut Vec<Op>) {
      let SlotKind::Target(target) = self.slots[index].kind else {
         return;
      };
      let place = self.slots[index].place.clone();
      let contributions = self
         .contributions
         .iter()
         .filter(|contribution| contribution.target == target.drawn_by())
         .collect::<Vec<_>>();
      for contribution in &contributions {
         let key = (contribution.id, place.clone());
         if (target.evaluates() || contribution.conditional)
            && !self.results.contains_key(&key)
            && self.pending.insert(key)
         {
            queue(Job::Evaluate(contribution.id, place.clone()));
         }
      }
      let ids = contributions.iter().map(|contribution| contribution.id).collect::<Vec<_>>();
      let drafts = self
         .templates
         .draw(target, &contributions, &place, &|contribution| {
            self.results.get(&(contribution.id, place.clone())).cloned()
         });
      let mut roots = Vec::new();
      let mut all = Vec::new();
      for mut draft in drafts {
         if target == Target::TabPage {
            self.restore_typed(&mut draft, &place);
         }
         let root = self.emit(draft, None, out, &mut all);
         if target == Target::Tab
            && let [.., text, _, bar, _] = all[..]
         {
            // Kids come first: text, inner, bar, then the tab.
            self.tab_parts.insert(root, (text, bar));
         }
         roots.push(root);
      }
      self.slots[index].roots = roots;
      self.slots[index].all = all;
      if target == Target::Tab {
         out.extend(self.tab_looks(self.slots[index].anchor));
      }
      // The content's tint, once no answer for it is outstanding.
      if target == Target::Content
         && !ids.iter().any(|id| self.pending.contains(&(*id, place.clone())))
      {
         let tint = ids.iter().find_map(|id| {
            match self.results.get(&(*id, place.clone())) {
               Some(Outcome::Text(Some(rendered))) => rendered.tint,
               _ => None,
            }
         });
         if let Place::Message(message) = place {
            self.set_tint(message, tint, out);
         }
      }
   }

   /// Colors the author and text of `message`, or gives them back Phoenix's
   /// colors.
   fn set_tint(&mut self, message: String, tint: Option<i64>, out: &mut Vec<Op>) {
      let old = match tint {
         Some(color) => self.tints.insert(message.clone(), color),
         None => self.tints.remove(&message),
      };
      if old == tint {
         return;
      }
      for (id, (owner, props)) in &self.texts {
         if *owner != message {
            continue;
         }
         let props = match tint {
            Some(color) => recolored(props, color, true),
            None => {
               props
                  .iter()
                  .filter(|(key, _)| *key == Prop::COLOR || *key == SPANS)
                  .cloned()
                  .collect()
            },
         };
         out.push(Op::Update { id: *id, props });
      }
   }

   /// The message whose author or content text `id` is.
   fn text_message(&self, id: i32) -> Option<String> {
      let own = self.mirror.accessibility_id(id).and_then(|aid| aid.strip_prefix("message.author:"));
      let parent = self
         .mirror
         .parent(id)
         .and_then(|parent| self.mirror.accessibility_id(parent))
         .and_then(|aid| aid.strip_prefix("message.content:"));
      own.or(parent).map(str::to_owned)
   }

   fn tint_of(&self, id: i32) -> Option<i64> {
      self.texts.get(&id).and_then(|(message, _)| self.tints.get(message)).copied()
   }

   fn emit(
      &mut self,
      draft: Draft,
      inherited: Option<&Handler>,
      out: &mut Vec<Op>,
      all: &mut Vec<i32>,
   ) -> i32 {
      let handler = draft.press.or_else(|| inherited.cloned());
      let ids = draft
         .kids
         .into_iter()
         .map(|kid| self.emit(kid, handler.as_ref(), out, all))
         .collect::<Vec<_>>();
      let id = self.next_id;
      self.next_id += 1;
      if let Some(handler) = handler {
         self.handlers.insert(id, handler);
      }
      out.push(Op::Create {
         id,
         kind: draft.kind,
         props: draft.props,
      });
      if !ids.is_empty() {
         out.push(Op::Children { id, ids });
      }
      all.push(id);
      id
   }

   /// `parent`'s children as Rust has them, with the slots spliced in.
   fn merged(&self, parent: i32) -> Vec<i32> {
      let mut list = self.mirror.kids(parent).to_vec();
      for slot in self.slots.iter().filter(|slot| slot.parent == parent) {
         let at = match slot.before {
            Before::Start => 0,
            Before::Node(node) => list.iter().position(|id| *id == node).unwrap_or(list.len()),
            Before::End => list.len(),
         };
         list.splice(at..at, slot.roots.iter().copied());
      }
      list
   }

   fn forget_slot(&mut self, slot: &Slot) {
      self.forget_ids(&slot.all);
   }

   fn forget_ids(&mut self, ids: &[i32]) {
      for id in ids {
         self.handlers.remove(id);
         self.tab_parts.remove(id);
      }
   }

   /// Draws every slot of `target` anew, and opens slots at anchors that had
   /// none.
   fn refresh(&mut self, target: Target) -> Vec<Op> {
      let mut out = Vec::new();
      let mut deletes = Vec::new();
      let mut touched = HashSet::new();
      for index in 0..self.slots.len() {
         if self.slots[index].kind != SlotKind::Target(target) {
            continue;
         }
         let old = std::mem::take(&mut self.slots[index].all);
         self.forget_ids(&old);
         deletes.extend(old);
         self.fill(index, &mut out);
         touched.insert(self.slots[index].parent);
      }
      let anchors = self
         .anchors
         .get(&target)
         .map_or_else(Vec::new, |anchors| anchors.iter().copied().collect());
      for anchor in anchors {
         if let Some(parent) = self.place(target, anchor, &mut out) {
            touched.insert(parent);
         }
      }
      self.finish(out, touched, deletes)
   }

   /// Records a plugin's answer and redraws the slots it was for.
   fn evaluated(
      &mut self,
      contribution: ContributionId,
      place: Place,
      outcome: Outcome,
   ) -> Vec<Op> {
      let key = (contribution, place);
      self.pending.remove(&key);
      let Some(target) = self
         .contributions
         .iter()
         .find(|known| known.id == contribution)
         .map(|known| known.target)
      else {
         return Vec::new();
      };
      if self.results.len() >= RESULTS_CAP {
         self.results.clear();
      }
      let shows = |outcome: &Outcome| match outcome {
         Outcome::Text(rendered) => rendered.is_some(),
         Outcome::Badges(badges) => !badges.is_empty(),
         Outcome::Shown(shown) => *shown,
         Outcome::Page(_) => true,
      };
      let visible = shows(&outcome);
      let place = key.1.clone();
      let was = self.results.insert(key, outcome);
      if !visible && !was.as_ref().is_some_and(shows) {
         return Vec::new();
      }
      let mut out = Vec::new();
      let mut deletes = Vec::new();
      let mut touched = HashSet::new();
      for index in 0..self.slots.len() {
         let slot = &self.slots[index];
         if !matches!(slot.kind, SlotKind::Target(drawn) if drawn.drawn_by() == target)
            || slot.place != place
         {
            continue;
         }
         let old = std::mem::take(&mut self.slots[index].all);
         self.forget_ids(&old);
         deletes.extend(old);
         self.fill(index, &mut out);
         touched.insert(self.slots[index].parent);
      }
      self.finish(out, touched, deletes)
   }

   fn finish(&self, mut out: Vec<Op>, touched: HashSet<i32>, deletes: Vec<i32>) -> Vec<Op> {
      for parent in touched {
         if self.mirror.has_kids(parent) {
            out.push(Op::Children {
               id:  parent,
               ids: self.merged(parent),
            });
         }
      }
      if !deletes.is_empty() {
         out.push(Op::Delete { ids: deletes });
      }
      out
   }
}

const SPANS: Prop = Prop::named("spans");

/// `props` with their text and every span in `color`. An update (`all` off)
/// only recolors what it sets.
fn recolored(props: &Props, color: i64, all: bool) -> Props {
   let mut props = props.clone();
   let key = Prop::COLOR.key();
   for (prop, value) in &mut props {
      if *prop != SPANS {
         continue;
      }
      let Value::Arr(spans) = value else {
         continue;
      };
      for span in spans {
         let Value::Obj(entries) = span else {
            continue;
         };
         match entries.iter_mut().find(|(field, _)| *field == key) {
            Some((_, old)) => *old = Value::I64(color),
            None => entries.push((key, Value::I64(color))),
         }
      }
   }
   if all || props.iter().any(|(prop, _)| *prop == Prop::COLOR) {
      set_prop(&mut props, Prop::COLOR, Some(Value::I64(color)));
   }
   props
}

const HIDDEN: Prop = Prop::named("hidden");

fn number(value: &Value) -> Option<f64> {
   match value {
      Value::F64(number) => Some(*number),
      Value::I32(number) => Some(f64::from(*number)),
      _ => None,
   }
}
const BACKGROUND: Prop = Prop::named("background");

/// Turns `pager` to `page`. Rust counts its own `seq` up from 1, and the host
/// takes any new one.
fn page_op(pager: i32, page: usize) -> Op {
   static SEQUENCE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(1 << 30);
   Op::Update {
      id:    pager,
      props: vec![(
         PAGE,
         Value::Obj(vec![
            (Prop::named("seq").key(), Value::I32(SEQUENCE.fetch_add(1, Ordering::Relaxed))),
            (Prop::INDEX.key(), Value::I32(i32::try_from(page).unwrap_or_default())),
            (Prop::named("animated").key(), Value::Bool(true)),
         ]),
      )],
   }
}

/// The user a profile batch is about: the id in the avatar image under the
/// node `Avatar`. A default avatar has none, and then it is the profile
/// Phoenix just fetched. Other avatars in the batch (mutual friends) are
/// never taken for it.
fn profile_user(ops: &[Op]) -> Option<String> {
   let avatar = ops.iter().find_map(|op| {
      match op {
         Op::Create { id, props, .. }
            if matches!(prop(props, Prop::ACCESSIBILITY_ID), Some(Value::Str(aid)) if aid == "Avatar") =>
         {
            Some(*id)
         },
         _ => None,
      }
   });
   let image = avatar.and_then(|avatar| {
      ops.iter().find_map(|op| {
         match op {
            Op::Create { id, kind, props } if *id > avatar && *kind == ViewKind::Image => {
               match prop(props, Prop::URI) {
                  Some(Value::Str(uri)) => Some(uri.clone()),
                  _ => None,
               }
            },
            _ => None,
         }
      })
   });
   image
      .as_deref()
      .and_then(avatar_user)
      .or_else(crate::discord::last_profile)
}

/// The user id in an avatar URL, `…/avatars/<id>/<hash>` or
/// `…/users/<id>/avatars/<hash>` for a server avatar.
fn avatar_user(uri: &str) -> Option<String> {
   let digits = |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
   if let Some(at) = memmem::find(uri.as_bytes(), b"/users/") {
      let id = uri[at + "/users/".len()..].split('/').next()?;
      return digits(id).then(|| id.to_owned());
   }
   let at = memmem::find(uri.as_bytes(), b"/avatars/")?;
   let id = uri[at + "/avatars/".len()..].split('/').next()?;
   digits(id).then(|| id.to_owned())
}

/// The message id in `message.{content,avatar,author,timestamp}:<id>`.
fn message_id(aid: &str) -> Option<&str> {
   let (kind, id) = aid.strip_prefix("message.")?.split_once(':')?;
   (matches!(kind, "content" | "avatar" | "author" | "timestamp")
      && !id.is_empty()
      && id.bytes().all(|byte| byte.is_ascii_digit()))
   .then_some(id)
}
