use std::{collections::HashSet, str::FromStr, time::Instant};

use anyhow::{Result, bail};
use archipelago_rs as ap;
use archipelago_rs::RichText;
use fromsoftware_shared::FromStatic;
use log::*;
use regex_macro::regex;
use sekiro::sprj::*;

use crate::item::{EquipParamExt, ItemIdExt};
use crate::save_data::*;
use crate::slot_data::{I64Key, SlotData};
use shared::{Core as SharedCore, CoreBase};

/// The core of the Archipelago mod. This is responsible for running the
/// non-UI-related game logic and interacting with the Archipelago client.
pub struct Core {
    /// The cross-game core.
    base: CoreBase<SlotData>,

    /// The time we last granted an item to the player. Used to ensure we don't
    /// give more than one item per second.
    last_item_time: Instant,

    /// The number of locations sent to the server in this session. This always
    /// starts at 0 when the player boots the game again to ensure that they
    /// resend any locations that may have been missed.
    locations_sent: usize,

    /// Whether the player has achieved their goal and sent that information to
    /// the Archipelago server. This is stored here rather than in the save data
    /// so that it's resent every time the player starts the game, just in case
    /// it got lost in transit.
    sent_goal: bool,
}

impl shared::Core for Core {
    type SlotData = SlotData;
    type Game = crate::game::Sekiro;

    /// Creates a new instance of the mod.
    fn new() -> Result<Self> {
        Ok(Self {
            base: CoreBase::new("Sekiro: Shadows Die Twice")?,
            last_item_time: Instant::now(),
            locations_sent: 0,
            sent_goal: false,
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
        self.check_seed_conflict()?;
        if let Some(save_data) = SaveData::instance_mut().as_mut()
            && save_data.seed.is_none()
        {
            save_data.seed = Some(self.seed().to_string());
        };

        // Process events that should only happen when the player has a save
        // loaded and is actively playing.
        self.take_events();

        self.process_incoming_items();
        self.process_inventory_items()?;
        self.handle_goal()?;

        Ok(())
    }

    fn handle_command(&mut self, command: &str, arg: Option<&str>) -> bool {
        let mut arg_error = |usage: &str| {
            self.log(vec![
                RichText::Color {
                    text: format!("Invalid {}.", command),
                    color: ap::TextColor::Red,
                },
                " Usage:\n".into(),
                usage.into(),
            ]);
        };

        match command {
            "!getevent" => {
                let Some(flag) = arg.and_then(|f| u32::from_str(f).ok()) else {
                    arg_error("!getevent EVENT_FLAG");
                    return true;
                };

                let Ok(flag) = EventFlag::try_from(flag) else {
                    self.log(RichText::Color {
                        text: format!("Invalid event ID: {}", flag),
                        color: ap::TextColor::Red,
                    });
                    return true;
                };

                let Ok(events) = (unsafe { SprjEventFlagMan::instance() }) else {
                    self.log(RichText::Color {
                        text: "SprjEventFlagMan not loaded".into(),
                        color: ap::TextColor::Red,
                    });
                    return true;
                };

                let value = events.get_flag(flag);
                self.log(vec![
                    "Event ".into(),
                    RichText::Color {
                        text: format!("{:?}", u32::from(flag)),
                        color: ap::TextColor::Blue,
                    },
                    ": ".into(),
                    RichText::Color {
                        text: format!("{:?}", value),
                        color: if value {
                            ap::TextColor::Green
                        } else {
                            ap::TextColor::Red
                        },
                    },
                ]);

                true
            }

            #[cfg(debug_assertions)]
            "!setevent" => {
                let Some((flag, value)) = arg.and_then(|a| {
                    let args = regex!(" +").split(a).collect::<Vec<_>>();
                    if args.len() == 2 {
                        Some((u32::from_str(args[0]).ok()?, bool::from_str(args[1]).ok()?))
                    } else {
                        None
                    }
                }) else {
                    arg_error("!setevent EVENT_FLAG BOOL");
                    return true;
                };

                let Ok(flag) = EventFlag::try_from(flag) else {
                    self.log(RichText::Color {
                        text: format!("Invalid event ID: {}", flag),
                        color: ap::TextColor::Red,
                    });
                    return true;
                };

                let Ok(events) = (unsafe { SprjEventFlagMan::instance() }) else {
                    self.log(RichText::Color {
                        text: "SprjEventFlagMan not loaded".into(),
                        color: ap::TextColor::Red,
                    });
                    return true;
                };

                events.set_flag(flag, value);
                self.log(vec![
                    "Set event ".into(),
                    RichText::Color {
                        text: format!("{:?}", u32::from(flag)),
                        color: ap::TextColor::Blue,
                    },
                    " to ".into(),
                    RichText::Color {
                        text: format!("{:?}", value),
                        color: if value {
                            ap::TextColor::Green
                        } else {
                            ap::TextColor::Red
                        },
                    },
                ]);

                true
            }

            _ => false,
        }
    }
}

impl Core {
    /// Returns an error if there's a conflict between the notion of the current
    /// seed in the server, the save, and/or the config. Also updates the save
    /// data's notion based on whatever is available if it doesn't exist yet.
    fn check_seed_conflict(&mut self) -> Result<()> {
        let client_seed = self.client().map(|c| c.seed_name());
        let save = SaveData::instance();
        let save_seed = save.as_ref().and_then(|s| s.seed.as_ref());

        match (client_seed, save_seed) {
            (Some(client_seed), _) if client_seed != self.seed() => bail!(
                "You've connected to a different Archipelago multiworld than the one that \
                 SekiroRandomizer.exe used!\n\
                 \n\
		 Connected room seed: {}\n\
                 SekiroRandomizer.exe seed: {}",
                client_seed,
                self.seed()
            ),
            (Some(client_seed), Some(save_seed)) if client_seed != save_seed => bail!(
                "You've connected to a different Archipelago multiworld than the one that \
                 you used before with this save!\n\
                 \n\
		 Connected room seed: {}\n\
		 Save file seed: {}",
                client_seed,
                save_seed
            ),
            (_, Some(save_seed)) if self.seed() != save_seed => bail!(
                "Your most recent SekiroRandomizer.exe invocation connected to a different \
                 Archipealgo multiworld than the one that you used before with this save!\n\
                 \n\
                 SekiroRandomizer.exe seed: {}\n\
                 Save file seed: {}",
                self.seed(),
                save_seed
            ),
            _ => Ok(()),
        }
    }

    /// Handle new items, distributing them to the player when appropriate. This
    /// also initializes the [SaveData] for a new file.
    fn process_incoming_items(&mut self) {
        let Some(client) = self.client() else {
            return;
        };
        let Ok(item_man) = (unsafe { MapItemMan::instance() }) else {
            return;
        };
        let mut save_data = SaveData::instance_mut();
        let Some(save_data) = save_data.as_mut() else {
            return;
        };

        // Wait a second between each item grant.
        if self.last_item_time.elapsed().as_secs() < 1 {
            return;
        }

        if let Some(item) = client.received_items().first() {
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

            save_data.items_granted += 1;
            self.last_item_time = Instant::now();
        }
    }

    /// Removes any placeholder items from the player's inventory and notifies
    /// the server that they've been accessed.
    fn process_inventory_items(&mut self) -> Result<()> {
        let Some(ref mut save_data) = SaveData::instance_mut() else {
            return Ok(());
        };
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
                grant_item_without_notifying(real_id, quantity)?;
            } else {
                // Presumably any item without local item data is a foreign
                // item, but we'll log a bunch of extra data in case there's a
                // bug we need to track down.
                info!(
                    "  Item has no local item data. Sale value: {}, sell value: {}",
                    row.sale_value(),
                    row.sell_value()
                );
            }
            info!("  Removing from inventory");
            game_data_man.remove_item(id, 1);
        }

        if save_data.locations.len() > self.locations_sent
            && let Some(client) = self.client_mut()
        {
            client.mark_checked(save_data.locations.iter().copied())?;
            self.locations_sent = save_data.locations.len();
        }
        Ok(())
    }

    /// Detects when the player has won the game and notifies the server.
    fn handle_goal(&mut self) -> Result<()> {
        if let Ok(event_man) = (unsafe { SprjEventFlagMan::instance() })
            && !self.sent_goal
            && let Some(client) = self.client_mut()
            // Defeated Sword Saint Isshin
            && event_man.get_flag(11125706.try_into().unwrap())
            // Beat the game (gave tears to Kuro or completed Shura)
            && event_man.get_flag(6901.try_into().unwrap())
        {
            client.set_status(ap::ClientStatus::Goal)?;
            self.sent_goal = true;
        }

        Ok(())
    }
}

/// Gives the player `quantity` copies of `item` without popping up a
/// notification on screen.
fn grant_item_without_notifying(id: ItemId, quantity: u32) -> Result<()> {
    let (old_notice_log, old_notice_dialog) = set_notice(id, false, false)?;
    unsafe { MapItemMan::instance() }?.grant_item(ItemBufferEntry::new(id, quantity));
    set_notice(id, old_notice_log, old_notice_dialog)?;
    Ok(())
}

/// Sets `item`'s `notice_log` and `notice_dialog` flags, which control how it's
/// displayed, to the given values and returns the previous values.
fn set_notice(id: ItemId, notice_log: bool, notice_dialog: bool) -> Result<(bool, bool)> {
    let row = unsafe { SoloParamRepository::instance() }?
        .get_equip_param_mut(id)
        .unwrap_or_else(|| panic!("no row for item ID {:?}", id))
        .into_dyn();
    let previous = (row.is_notice_log(), row.is_notice_dialog());
    row.set_is_notice_log(notice_log);
    row.set_is_notice_log(notice_dialog);
    Ok(previous)
}
