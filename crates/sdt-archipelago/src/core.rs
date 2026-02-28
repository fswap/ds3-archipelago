use std::collections::HashSet;
use std::time::Instant;

use anyhow::Result;
use sekiro::{param::*, sprj::*};
use fromsoftware_shared::FromStatic;
use log::*;

use crate::item::{EquipParamExt, ItemIdExt};
use crate::slot_data::{I64Key, SlotData};
use shared::{Core as SharedCore, CoreBase};

/// The core of the Archipelago mod. This is responsible for running the
/// non-UI-related game logic and interacting with the Archieplago client.
pub struct Core {
    /// The cross-game core.
    base: CoreBase<SlotData>,

    /// The time we last granted an item to the player. Used to ensure we don't
    /// give more than one item per second.
    last_item_time: Instant,
}

impl shared::Core for Core {
    type SlotData = SlotData;

    /// Creates a new instance of the mod.
    fn new() -> Result<Self> {
        Ok(Self {
            base: CoreBase::new()?,
            last_item_time: Instant::now(),
        })
    }

    fn base(&self) -> &CoreBase<SlotData> {
        &self.base
    }

    fn base_mut(&mut self) -> &mut CoreBase<SlotData> {
        &mut self.base
    }

    /// Updates the game logic and checks for common errors. This does nothing
    /// if we're not currently connected to the Archipelago server or if the mod
    /// has encountered a fatal error.
    fn update_live(&mut self) -> Result<()> {
        // Process events that should only happen when the player has a save
        // loaded and is actively playing.
        self.take_events();

        self.process_incoming_items();
        self.process_inventory_items()?;

        Ok(())
    }
}

impl Core {
    /// Handle new items, distributing them to the player when appropriate.
    fn process_incoming_items(&mut self) {
        let Some(client) = self.client() else {
            return;
        };
        let Ok(item_man) = (unsafe { MapItemMan::instance() }) else {
            return;
        };

        // Wait a second between each item grant.
        if self.last_item_time.elapsed().as_secs() < 1 {
            return;
        }

        if let Some(item) = client
            .received_items()
            .first()
        {
            let id_key = I64Key(item.item().id());
            let sdt_id = client
                .slot_data()
                .ap_ids_to_item_ids
                .get(&id_key)
                .unwrap_or_else(|| {
                    panic!(
                        "Archipelago item {:?} should have a SDT ID defined in slot data",
                        item.item()
                    )
                })
                .0;
            let quantity = client
                .slot_data()
                .item_counts
                .get(&id_key)
                .copied()
                .unwrap_or(1);

            info!(
                "Granting {} (AP ID {}, SDT ID {:?} from {})",
                item.item().name(),
                item.item().id(),
                sdt_id,
                item.location().name()
            );

            item_man.grant_item(ItemBufferEntry::new(sdt_id, quantity));

            self.last_item_time = Instant::now();
        }
    }

    /// Removes any placeholder items from the player's inventory and notifies
    /// the server that they've been accessed.
    fn process_inventory_items(&mut self) -> Result<()> {
        let Ok(game_data_man) = (unsafe { GameDataMan::instance() }) else {
            return Ok(());
        };
        let Ok(solo_params) = (unsafe { SoloParamRepository::instance() }) else {
            return Ok(());
        };

        // We have to make a separate vector here so we aren't borrowing while
        // we make mutations.
        let ids = game_data_man
            .local_player
            .equip_game_data
            .equip_inventory_data
            .items_data
            .items()
            .map(|e| e.item_id)
            .collect::<Vec<_>>();
        let mut locations = HashSet::<i64>::new();
        for id in ids {
            if !id.is_archipelago() {
                continue;
            }

            info!("Inventory contains Archipelago item {:?}", id);
            let row = solo_params
                .get_equip_param(id)
                .unwrap_or_else(|| panic!("no row defined for Archipelago ID {:?}", id));
            let row = row.as_dyn();

            info!("  Archipelago location: {}", row.archipelago_location_id());
            locations.insert(row.archipelago_location_id());

            if let Some((real_id, quantity)) = row.archipelago_item() {
                info!("  Converting to {}x {:?}", quantity, real_id);
                // game_data_man.give_item_directly(real_id, quantity);
            } else {
                // Presumably any item without local item data is a foreign
                // item, but we'll log a bunch of extra data in case there's a
                // bug we need to track down.
                info!(
                    "  Item has no local item data. QWC ID: {}, sell value: {}",
                    row.qwc_id(),
                    row.sell_value()
                );
            }
            info!("  Removing from inventory");
            game_data_man.remove_item(id, 1);
        }

        if let Some(client) = self.client_mut()
        {
            client.mark_checked(locations)?;
        }
        Ok(())
    }
}
