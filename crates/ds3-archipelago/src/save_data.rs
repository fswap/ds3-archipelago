use std::collections::HashSet;
use std::sync::{LazyLock, RwLock, RwLockReadGuard, RwLockWriteGuard};

use bincode::{Decode, Encode};
use darksouls3::sprj::MapItemMan;
use darksouls3_extra::save;
use fromsoftware_shared::FromStatic;
use log::*;

/// The singleton instance of the save data, or None if it hasn't been loaded
/// from the save file or set explicitly.
static INSTANCE: LazyLock<RwLock<SaveData>> = LazyLock::new(|| RwLock::new(Default::default()));

/// The configuration for the binary encoding of the save data.
const CONFIG: bincode::config::Configuration = bincode::config::standard();

/// Data that's saved and loaded along with the player's game save.
#[derive(Debug, Decode, Encode, Default)]
pub struct SaveData {
    /// The number of Archipelago items that have been granted to this player
    /// from foreign games throughout the course of this run.
    pub items_granted: usize,

    /// The set of Archipelago locations that this player has accessed so far in
    /// this game. We don't strictly need to track this, but it helps us avoid
    /// being overly chatty with the server.
    pub locations: HashSet<i64>,

    /// The Archipelago seed this save file was last connected to. This is used
    /// to verify that the player doesn't accidentally corrupt a save by loading
    /// into it while connected to the wrong multiworld.
    pub seed: Option<String>,

    /// The number of deaths that player has experienced since last sending a
    /// death link.
    pub deaths: u8,
}

impl SaveData {
    /// Register hooks for loading and unloading saves. These hooks are never
    /// unregistered.
    ///
    /// Safety: Follow all ilhook safety guidelines.
    pub unsafe fn hook() {
        unsafe {
            std::mem::forget(save::on_save_load(
                || {
                    Self::instance().and_then(|data| match bincode::encode_to_vec(&*data, CONFIG) {
                        Ok(bytes) => Some(bytes),
                        Err(err) => {
                            warn!("Failed to encode save data: {}", err);
                            None
                        }
                    })
                },
                |load_type| {
                    use save::OnLoadType::*;
                    let bytes = match load_type {
                        SavedData(bytes) => bytes,
                        MainMenu => {
                            // If the player goes back to the main menu, reset
                            // the granted items and seed info so that if the
                            // user starts a new file they get all new items and
                            // no seed conflict.
                            let mut save = INSTANCE.write().unwrap();
                            save.items_granted = 0;
                            save.seed = None;
                            return;
                        }
                        _ => return,
                    };

                    match bincode::decode_from_slice(&bytes, CONFIG) {
                        Ok((data, size)) => {
                            if size == bytes.len() {
                                *INSTANCE.write().unwrap() = data;
                            } else {
                                warn!(
                                    "Archipelago save data had {} extra bytes! This probably means \
                                 that you tried to load a save file created by a different version \
                                 of the Archipelago mod, or by a different mod entirely.",
                                    bytes.len() - size
                                );
                            }
                        }
                        Err(err) => warn!("Failed to load save data: {}", err),
                    }
                },
            ));
        }
    }

    /// Returns a read-only reference to the singleton [SaveData], or None if
    /// the player isn't currently loaded into a game.
    pub fn instance<'a>() -> Option<RwLockReadGuard<'a, Self>> {
        // MapItemMan is only instantiated when the player is loaded into an
        // actual game, *not* on the main menu. It's a more reliable way to
        // distinguish than whether a safe file has been loaded, because no file
        // is loaded when the player starts a new game.
        //
        // Safety: We don't actually use the man, we just check whether it
        // exists.
        if unsafe { MapItemMan::instance() }.is_ok() {
            Some(INSTANCE.read().unwrap())
        } else {
            None
        }
    }

    /// Returns a read-only reference to the singleton [SaveData], or None if
    /// the player isn't currently loaded into a game.
    pub fn instance_mut<'a>() -> Option<RwLockWriteGuard<'a, Self>> {
        // See above.
        if unsafe { MapItemMan::instance() }.is_ok() {
            Some(INSTANCE.write().unwrap())
        } else {
            None
        }
    }
}
