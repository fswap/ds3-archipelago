use anyhow::Result;

use crate::{Core, InputBlocker};

/// A trait that encapsulates specific behavior for each individual game that's
/// used by the shared library. We try to keep this minimal, with most game
/// interactions being left in the individual game mod crates.
pub trait Game: Send + Sync + 'static {
    /// This game's core mod type.
    type Core: Core;

    /// The hudhook type for this game's graphics implementation.
    type GraphicsHooks: hudhook::Hooks;

    /// The input blocker type to block input to this game.
    type InputBlocker: InputBlocker;

    /// Schedules `task` to be run each frame, ideally at the beginning of the
    /// frame, on the game's main thread.
    ///
    /// ## Safety
    ///
    /// This must be called on the main thread when no other references exist to
    /// the game's internal state.
    unsafe fn run_recurring_task(task: impl FnMut() + 'static + Send) -> Result<()>;

    /// Blocks until the core of the underlying game systems are initialized.
    fn wait_for_system_init() -> Result<()>;

    /// Returns whether the game is currently showing the main menu (or earlier
    /// during the initial load process).
    ///
    /// ## Safety
    ///
    /// This must be called on the main thread when no other references exist to
    /// the game's internal state.
    unsafe fn is_main_menu() -> bool;

    /// Forces the cursor to be visible on-screen.
    ///
    /// By default, does nothing.
    ///
    /// ## Safety
    ///
    /// This must be called on the main thread when no other references exist to
    /// the game's internal state.
    unsafe fn force_cursor_visible() {}

    /// Returns whether the player is currently in a menu, as opposed to
    /// actively playing the game.
    ///
    /// By default, this always returns false.
    ///
    /// ## Safety
    ///
    /// This must be called on the main thread when no other references exist to
    /// the game's internal state.
    unsafe fn is_menu_open() -> bool {
        false
    }
}
