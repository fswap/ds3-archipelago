use std::collections::HashSet;
use std::time::{Duration, Instant, SystemTime};
use std::{mem, str::FromStr};

use anyhow::{Result, bail};
use archipelago_rs::{self as ap, RichText};
use darksouls3::{app_menu::*, cs::*, param::*, sprj::*};
use fromsoftware_shared::{FromStatic, Superclass};
use log::*;
use regex_macro::regex;

use crate::item::{EquipParamExt, ItemIdExt};
use crate::save_data::*;
use crate::slot_data::{DeathLinkOption, I64Key, SlotData};
use shared::{Core as SharedCore, CoreBase};

/// The grace period after either sending or receiving a death link during which
/// no further death links will be sent or received.
const DEATH_LINK_GRACE_PERIOD: Duration = Duration::from_secs(30);

/// The core of the Archipelago mod. This is responsible for running the
/// non-UI-related game logic and interacting with the Archieplago client.
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

    /// The set of DS3 item IDs for shop locations whose hints have already been
    /// sent to the server. This is intentionally not preserved across loads so
    /// that if something goes wrong, the player can quit out and re-send hints.
    shop_items_hinted: HashSet<ItemId>,

    /// The last time the player either sent or received a death link (or
    /// started a session).
    last_death_link: Instant,

    /// Whether the player has achieved their goal and sent that information to
    /// the Archipelago server. This is stored here rather than in the save data
    /// so that it's resent every time the player starts the game, just in case
    /// it got lost in transit.
    sent_goal: bool,
}

impl shared::Core for Core {
    type SlotData = SlotData;

    /// Creates a new instance of the mod.
    fn new() -> Result<Self> {
        Ok(Self {
            base: CoreBase::new()?,
            last_item_time: Instant::now(),
            locations_sent: 0,
            shop_items_hinted: Default::default(),
            last_death_link: Instant::now(),
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

        self.check_dlc_error()?;

        // Process events that should only happen when the player has a save
        // loaded and is actively playing.
        use ap::Event::*;
        for event in self.take_events() {
            if let DeathLink { source, time, .. } = event {
                self.receive_death_link(source, time)
            }
        }

        self.send_death_link()?;
        self.process_incoming_items();
        self.process_inventory_items()?;
        self.send_shop_hints()?;
        self.handle_goal()?;

        Ok(())
    }

    /// Implementors may override this to handles custom command inputs via the
    /// say console. Returns whether a command was handled.
    ///
    /// By default, this doesn't handle any commands.
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
                        // TODO: Use `u32::from()` once EventFlag supports it
                        text: format!("{:?}", unsafe { mem::transmute::<EventFlag, u32>(flag) }),
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
                        // TODO: Use `u32::from()` once EventFlag supports it
                        text: format!("{:?}", unsafe { mem::transmute::<EventFlag, u32>(flag) }),
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
                 DS3Randomizer.exe used!\n\
                 \n\
		 Connected room seed: {}\n\
                 DS3Randomizer.exe seed: {}",
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
                "Your most recent DS3Randomizer.exe invocation connected to a different \
                 Archipealgo multiworld than the one that you used before with this save!\n\
                 \n\
                 DS3Randomizer.exe seed: {}\n\
                 Save file seed: {}",
                self.seed(),
                save_seed
            ),
            _ => Ok(()),
        }
    }

    /// Returns an error if [config] expects DLC to be installed and it is not.
    fn check_dlc_error(&self) -> Result<()> {
        if let Ok(dlc) = (unsafe { CSDlc::instance() }) &&
            // The DLC always registers as not installed until the player clicks
            // through the initial opening screen and loads their global save
            // data. Ideally we should find a better way of detecting when that
            // happens, but for now we just wait to indicate an error until
            // they're actually in a game.
            (unsafe { MapItemMan::instance() }).is_ok() &&
            self.client().is_some_and(|c|
            c.slot_data().options.enable_dlc)
            && (!dlc.dlc1_installed || !dlc.dlc2_installed)
        {
            bail!(
                "DLC is enabled for this seed but your game is missing {}.",
                if dlc.dlc1_installed {
                    "the Ringed City DLC"
                } else if dlc.dlc2_installed {
                    "the Ashes of Ariandel DLC"
                } else {
                    "both DLCs"
                }
            );
        } else {
            Ok(())
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
        let Ok(player_game_data) = (unsafe { PlayerGameData::instance() }) else {
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

        if let Some(item) = client
            .received_items()
            .iter()
            .find(|item| item.index() >= save_data.items_granted)
        {
            let id_key = I64Key(item.item().id());
            let ds3_id = client
                .slot_data()
                .ap_ids_to_item_ids
                .get(&id_key)
                .unwrap_or_else(|| {
                    panic!(
                        "Archipelago item {:?} should have a DS3 ID defined in slot data",
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
                "Granting {} (AP ID {}, DS3 ID {:?} from {})",
                item.item().name(),
                item.item().id(),
                ds3_id,
                item.location().name()
            );

            // Grant Path of the Dragon as a gesture rather than an item.
            if ds3_id.category() == ItemCategory::Goods && ds3_id.param_id() == 9030 {
                player_game_data.grant_gesture(29, ds3_id);
            } else {
                item_man.grant_item(ItemBufferEntry {
                    id: ds3_id,
                    quantity,
                    durability: -1,
                });
            }

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
        let Ok(regulation_manager) = (unsafe { CSRegulationManager::instance() }) else {
            return Ok(());
        };

        // We have to make a separate vector here so we aren't borrowing while
        // we make mutations.
        let ids = game_data_man
            .main_player_game_data
            .equipment
            .equip_inventory_data
            .items_data
            .items()
            .map(|e| e.item_id)
            .collect::<Vec<_>>();
        for id in ids {
            if !id.is_archipelago() {
                continue;
            }

            info!("Inventory contains Archipelago item {:?}", id);
            let row = regulation_manager
                .get_equip_param(id)
                .unwrap_or_else(|| panic!("no row defined for Archipelago ID {:?}", id));
            let row = row.as_dyn();

            info!("  Archipelago location: {}", row.archipelago_location_id());
            save_data.locations.insert(row.archipelago_location_id());

            if let EquipParamStruct::EQUIP_PARAM_GOODS_ST(good) = row.as_enum()
                && good.icon_id() == 7039
            {
                info!("  Item is Path of the Dragon, granting gesture");
                // If the player gets the synthetic Path of the Dragon item,
                // give them the gesture itself instead. Don't display an
                // item pop-up, because they already saw one when they got
                // the item.
                game_data_man
                    .main_player_game_data
                    .gesture_data
                    .set_gesture_acquired(29, true);
            } else if let Some((real_id, quantity)) = row.archipelago_item() {
                info!("  Converting to {}x {:?}", quantity, real_id);
                game_data_man.give_item_directly(real_id, quantity);
            } else {
                // Presumably any item without local item data is a foreign
                // item, but we'll log a bunch of extra data in case there's a
                // bug we need to track down.
                info!(
                    "  Item has no local item data. Basic price: {}, sell value: {}{}",
                    row.basic_price(),
                    row.sell_value(),
                    if let EquipParamStruct::EQUIP_PARAM_GOODS_ST(good) = row.as_enum() {
                        format!(", icon id: {}", good.icon_id())
                    } else {
                        "".into()
                    }
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

    /// Kills the player after a death link is received.
    fn receive_death_link(&mut self, source: String, time: SystemTime) {
        if !self.allow_death_link() {
            return;
        }
        if self
            .client()
            .is_none_or(|c| c.this_player().name() == source)
        {
            return;
        }

        let last_death_link_time = SystemTime::now() - self.last_death_link.elapsed();
        match time.duration_since(last_death_link_time) {
            Ok(dur) if dur < DEATH_LINK_GRACE_PERIOD => return,
            // An error means that the last death link was *after* [time].
            Err(_) => return,
            _ => {}
        }

        let Ok(player) = (unsafe { PlayerIns::instance() }) else {
            return;
        };

        // Always ignore death links that we sent.
        player.kill();
        self.last_death_link = Instant::now();
    }

    /// If a shop is currently open, send all its locations as hints to the
    /// server.
    fn send_shop_hints(&mut self) -> Result<()> {
        let Ok(regulation_manager) = (unsafe { CSRegulationManager::instance() }) else {
            return Ok(());
        };
        let Ok(nms) = (unsafe { NewMenuSystem::instance() }) else {
            return Ok(());
        };
        let Some(menu) = nms
            .windows()
            .find_map(|w| w.as_subclass::<GaitemSelectMenu>())
        else {
            return Ok(());
        };

        let locations = menu
            .items()
            .filter(|i| i.id.is_archipelago() && self.shop_items_hinted.insert(i.id))
            .map(|i| {
                regulation_manager
                    .get_equip_param(i.id)
                    .unwrap_or_else(|| panic!("no row defined for Archipelago ID {:?}", i.id))
                    .as_dyn()
                    .archipelago_location_id()
            })
            .collect::<Vec<_>>();
        if !locations.is_empty()
            && let Some(client) = self.client_mut()
        {
            info!("Hinting location IDs: {:?}", locations);
            client.create_hints(locations)?;
        }
        Ok(())
    }

    /// Sends a death link notification when the player dies.
    fn send_death_link(&mut self) -> Result<()> {
        if !self.allow_death_link() {
            return Ok(());
        }
        let Some(client) = self.client_mut() else {
            return Ok(());
        };
        let Ok(player) = (unsafe { PlayerIns::instance() }) else {
            return Ok(());
        };
        if player.super_chr_ins.modules.data.hp != 0 {
            return Ok(());
        }
        let Some(mut save) = SaveData::instance_mut() else {
            return Ok(());
        };

        if client.slot_data().options.death_link != DeathLinkOption::LostSouls
            || unsafe { GameDataMan::instance() }.is_ok_and(|man| man.bloodstain.exists())
        {
            save.deaths += 1;
            let amnesty = client.slot_data().options.death_link_amnesty;
            if save.deaths >= amnesty {
                client.death_link(Default::default())?;
                save.deaths = 0;
                self.log("You have sent a death link to your teammates.");
            } else {
                let remaining = amnesty - save.deaths;
                self.log(format!(
                    "You have been granted death link amnesty. {}",
                    if remaining == 1 {
                        "1 death remains.".to_string()
                    } else {
                        format!("{} deaths remain.", remaining)
                    }
                ));
            }
        }

        // Set this even if we don't send out a death link so we don't run this
        // multiple times while the player is dying and so they don't get killed
        // from an incoming death link immediately after respawning.
        self.last_death_link = Instant::now();

        Ok(())
    }

    /// Returns whether death links (sending or receiving) are currently
    /// allowed.
    fn allow_death_link(&self) -> bool {
        let Some(client) = self.client() else {
            return false;
        };

        client.slot_data().options.death_link != DeathLinkOption::Off
            && self.last_death_link.elapsed() >= DEATH_LINK_GRACE_PERIOD
    }

    /// Detects when the player has won the game and notifies the server.
    fn handle_goal(&mut self) -> Result<()> {
        if let Ok(event_man) = (unsafe { SprjEventFlagMan::instance() })
            && !self.sent_goal
            && let Some(client) = self.client_mut()
            && client
                .slot_data()
                .goal
                .iter()
                .all(|flag| event_man.get_flag(*flag))
        {
            client.set_status(ap::ClientStatus::Goal)?;
            self.sent_goal = true;
        }

        Ok(())
    }
}
