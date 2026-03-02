/// A shared trait for structs that block inputs to the game itself.
pub trait InputBlocker: Send + Sync {
    /// Blocks all input from inputs selected by [InputFlags] and unblocks all
    /// input from inputs that aren't selected.
    ///
    /// This doesn't remove blocks added by the game itself (for example because
    /// the Steam overlay is active).
    fn block_only(&self, inputs: InputFlags);
}

bitflags::bitflags! {
    /// A bit flag that indicates a set of input methods to target.
    #[derive(Debug, Clone, Copy)]
    pub struct InputFlags: u8 {
        /// Input from a player's controller.
        const GamePad = 0b001;

        /// Input from a player's keyboard.
        const Keyboard = 0b010;

        /// Input from a player's mouse.
        const Mouse = 0b100;
    }
}

/// An [InputBlocker] that does nothing. Used for games whose input blocking
/// hasn't yet been reverse-engineered.
pub struct NoOpInputBlocker;

impl InputBlocker for NoOpInputBlocker {
    fn block_only(&self, _: InputFlags) {}
}
