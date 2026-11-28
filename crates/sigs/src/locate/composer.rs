//! The chat bar's button row, where `phoenix.ui.addComposerButton` draws.

use crate::{
   Found,
   index::Index,
};

/// The chat bar's button row render, the one function loading the gift
/// button wrapper's `gift-button.wrap` element id.
///
/// It takes five arguments and returns its `AnyElement` through the return
/// slot on every desktop.
pub fn composer_buttons(ix: &Index<'_>) -> Found<usize> {
   ix.literal(b"gift-button.wrap")
      .one("composer buttons")
      .map(|func| func.start)
}
