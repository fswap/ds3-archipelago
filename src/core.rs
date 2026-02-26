use std::collections::{HashSet, VecDeque};
use std::time::{Duration, Instant, SystemTime};
use std::{io, iter::ExactSizeIterator, mem};

use anyhow::{Error, Result, bail};
use archipelago_rs as ap;
use darksouls3::{app_menu::*, cs::*, param::*, sprj::*};
use fromsoftware_shared::{FromStatic, InstanceResult, Superclass};
use log::*;

use crate::item::{EquipParamExt, ItemIdExt};
use crate::slot_data::{DeathLinkOption, I64Key, SlotData};
use crate::{config::Config, save_data::*};

/// The maximum number of log messages to store.
///
/// This is relatively low because imgui is not very efficient about not
/// rendering the offscreen messages every frame, which can cause real slowdown
/// over long runs with chatty connections.
const LOG_BUFFER_LIMIT: usize = 200;

/// The core of the Archipelago mod. This is responsible for running the
/// non-UI-related game logic and interacting with the Archieplago client.
pub struct Core {
    /// The configuration for the current Archipelago connection. This is not
    /// guaranteed to be complete *or* accurate; it's the mod's responsibility
    /// to ensure it makes sense before actually interacting with an individual
    /// game.
    config: Config,

    /// The log of prints displayed in the overlay.
    log_buffer: VecDeque<ap::Print>,

    /// The Archipelago client connection.
    connection: ap::Connection<SlotData>,

    /// Events we're waiting to process until the player loads a save. This is
    /// always empty unless a connection is connected and the player is on the
    /// main menu (or in the initial waiting period during a load).
    event_buffer: Vec<ap::Event>,

    /// The time we last granted an item to the player. Used to ensure we don't
    /// give more than one item per second.
    last_item_time: Instant,

    /// The time at which we noticed the game loading (as indicated by
    /// MapItemMan coming into existence). Used to compute the grace period
    /// before we start doing stuff in game. None if the game is not currently
    /// loaded.
    load_time: Option<Instant>,

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

    /// The fatal error that this has encountered, if any. If this is not
    /// `None`, most in-game processing will be disabled.
    error: Option<Error>,
}

/// The grace period between MapItemMan starting to exist and the mod beginning
/// to take actions.
const GRACE_PERIOD: Duration = Duration::from_secs(10);

/// The grace period after either sending or receiving a death link during which
/// no further death links will be sent or received.
const DEATH_LINK_GRACE_PERIOD: Duration = Duration::from_secs(30);

impl Core {
    /// Creates a new instance of the mod.
    pub fn new() -> Result<Self> {
        let config = Config::load()?;
        let connection = Self::new_connection(&config);
        Ok(Self {
            config,
            connection,
            event_buffer: vec![],
            log_buffer: Default::default(),
            last_item_time: Instant::now(),
            load_time: None,
            locations_sent: 0,
            shop_items_hinted: Default::default(),
            last_death_link: Instant::now(),
            sent_goal: false,
            error: None,
        })
    }

    /// Creates a new [ClientConnection] based on the connection information in [config].
    fn new_connection(config: &Config) -> ap::Connection<SlotData> {
        let mut options = ap::ConnectionOptions::new()
            .receive_items(ap::ItemHandling::OtherWorlds {
                own_world: false,
                starting_inventory: true,
            })
            .tags(vec!["DeathLink"]);
        if let Some(password) = config.password() {
            options = options.password(password);
        }

        ap::Connection::new(config.url(), "Dark Souls III", config.slot(), options)
    }

    /// Returns the current connection type.
    pub fn connection_state_type(&self) -> ap::ConnectionStateType {
        self.connection.state_type()
    }

    /// Returns whether the current connection is disconnected.
    pub fn is_disconnected(&self) -> bool {
        self.connection.is_disconnected()
    }

    /// Returns the current user config.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Retries the Archipelago connection with the same information.
    pub fn reconnect(&mut self) {
        if self.connection_state_type() == ap::ConnectionStateType::Disconnected {
            self.log("Reconnecting...");
        }

        self.connection = Self::new_connection(&self.config);
    }

    /// Updates the URL to use to connect to Archipelago and reconnects the
    /// Archipelago session.
    pub fn update_url(&mut self, url: impl AsRef<str>) -> Result<()> {
        if self.connection_state_type() == ap::ConnectionStateType::Disconnected {
            self.log("Reconnecting...");
        }

        self.config.set_url(url);
        self.config.save()?;
        self.connection = Self::new_connection(&self.config);
        Ok(())
    }

    /// Returns a reference to the Archipelago client, if it's connected.
    pub fn client(&self) -> Option<&ap::Client<SlotData>> {
        self.connection.client()
    }

    /// Returns a mutable reference to the Archipelago client, if it's connected.
    pub fn client_mut(&mut self) -> Option<&mut ap::Client<SlotData>> {
        self.connection.client_mut()
    }

    /// Returns the list of all logs that have been emitted in the current
    /// session.
    pub fn logs(&self) -> impl ExactSizeIterator<Item = &ap::Print> {
        self.log_buffer.iter()
    }

    /// Runs the core logic of the mod. This may set [error], which should be
    /// surfaced to the user.
    pub fn update(&mut self) {
        self.update_always();
        if let Err(err) = self.update_live() {
            self.error = Some(err);
        }
    }

    /// If this client has encountered a fatal error, takes ownership of it.
    pub fn take_error(&mut self) -> Option<Error> {
        if let Some(err) = self.error.take() {
            self.error = Some(ap::Error::Elsewhere.into());
            Some(err)
        } else {
            None
        }
    }

    /// Updates the Archipelago connection, adds any events that need processing
    /// to [event_buffer].
    ///
    /// This is always run regardless of whether the client is connected or the
    /// mod has experienced a fatal error.
    fn update_always(&mut self) {
        use ap::Event::*;
        let mut state = self.connection.state_type();
        let mut events = self.connection.update();

        // Process events that should happen even when the player isn't in an
        // active save.
        for event in events.extract_if(.., |e| matches!(e, Connected | Error(_) | Print(_))) {
            match event {
                Connected => {
                    state = ap::ConnectionStateType::Connected;
                }
                Error(err) if err.is_fatal() => {
                    let err = self.connection.err();
                    self.log(
                        if let ap::Error::WebSocket(tungstenite::Error::Io(io)) = err
                            && matches!(
                                io.kind(),
                                io::ErrorKind::ConnectionRefused | io::ErrorKind::TimedOut
                            )
                        {
                            vec![
                                ap::RichText::Color {
                                    text: "Connection refused. ".into(),
                                    color: ap::TextColor::Red,
                                },
                                "Make sure the server session is running and the URL is \
                                 up-to-date."
                                    .into(),
                            ]
                        } else if state == ap::ConnectionStateType::Connected {
                            vec![
                                ap::RichText::Color {
                                    text: "Connection failed: ".into(),
                                    color: ap::TextColor::Red,
                                },
                                err.to_string().into(),
                            ]
                        } else {
                            vec![
                                ap::RichText::Color {
                                    text: "Disconnected: ".into(),
                                    color: ap::TextColor::Red,
                                },
                                err.to_string().into(),
                            ]
                        },
                    );
                    self.event_buffer.clear();
                }
                Error(err) => self.log(err.to_string()),
                Print(print) => {
                    info!("[APS] {print}");
                    if self.log_buffer.len() >= LOG_BUFFER_LIMIT {
                        self.log_buffer.pop_front();
                    }
                    self.log_buffer.push_back(print);
                }
                _ => {}
            }
        }

        if state == ap::ConnectionStateType::Connected {
            self.event_buffer.extend(events);
        } else {
            debug_assert!(self.event_buffer.is_empty());
        }
    }

    /// Updates the game logic and checks for common errors. This does nothing
    /// if we're not currently connected to the Archipelago server or if the mod
    /// has encountered a fatal error.
    fn update_live(&mut self) -> Result<()> {
        if self.connection.client().is_none() || self.error.is_some() {
            return Ok(());
        }

        let item_man = unsafe { MapItemMan::instance() };
        if item_man.is_err() {
            self.load_time = None;
        } else if self.load_time.is_none() {
            self.load_time = Some(Instant::now());
        }

        if let Some(time) = self.load_time
            && time.elapsed() < GRACE_PERIOD
        {
            return Ok(());
        }

        self.check_version_conflict()?;

        self.check_seed_conflict()?;
        if let Some(save_data) = SaveData::instance_mut().as_mut()
            && save_data.seed.is_none()
        {
            save_data.seed = Some(self.config.seed().to_string());
        };

        self.check_dlc_error()?;

        // Process events that should only happen when the player has a save
        // loaded and is actively playing.
        use ap::Event::*;
        for event in mem::take(&mut self.event_buffer) {
            if let DeathLink { source, time, .. } = event {
                self.receive_death_link(source, time)
            }
        }

        self.send_death_link()?;
        self.process_incoming_items(&item_man);
        self.process_inventory_items()?;
        self.send_shop_hints()?;
        self.handle_goal()?;

        Ok(())
    }

    /// Returns an error if the user's static randomizer version doesn't match
    /// this mod's version.
    fn check_version_conflict(&self) -> Result<()> {
        if let Some(client_version) = self.config().client_version()
            && client_version != env!("CARGO_PKG_VERSION")
        {
            bail!(
                "Your apconfig.json was generated using static randomizer v{}, but this client is \
                 v{}. Re-run the static randomizer with the current version.",
                client_version,
                env!("CARGO_PKG_VERSION"),
            );
        } else {
            Ok(())
        }
    }

    /// Returns an error if there's a conflict between the notion of the current
    /// seed in the server, the save, and/or the config. Also updates the save
    /// data's notion based on whatever is available if it doesn't exist yet.
    fn check_seed_conflict(&mut self) -> Result<()> {
        let client_seed = self.connection.client().map(|c| c.seed_name());
        let save = SaveData::instance();
        let save_seed = save.as_ref().and_then(|s| s.seed.as_ref());

        match (client_seed, save_seed) {
            (Some(client_seed), _) if client_seed != self.config.seed() => bail!(
                "You've connected to a different Archipelago multiworld than the one that \
                 DS3Randomizer.exe used!\n\
                 \n\
		 Connected room seed: {}\n\
                 DS3Randomizer.exe seed: {}",
                client_seed,
                self.config.seed()
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
            (_, Some(save_seed)) if self.config.seed() != save_seed => bail!(
                "Your most recent DS3Randomizer.exe invocation connected to a different \
                 Archipealgo multiworld than the one that you used before with this save!\n\
                 \n\
                 DS3Randomizer.exe seed: {}\n\
                 Save file seed: {}",
                self.config.seed(),
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
            self.connection.client().is_some_and(|c|
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
    fn process_incoming_items(&mut self, item_man: &InstanceResult<&mut MapItemMan>) {
        let Some(client) = self.connection.client() else {
            return;
        };
        let Ok(item_man) = item_man else {
            return;
        };
        let Ok(player_game_data) = (unsafe { PlayerGameData::instance() }) else {
            return;
        };
        let mut save_data = SaveData::instance_mut();
        let Some(save_data) = save_data.as_mut() else {
            return;
        };

        // Wait a second between each item grant, and 10 seconds after we load
        // in before we start granting items at all.
        if self.last_item_time.elapsed().as_secs() < 1
            || self.load_time.is_none_or(|i| i.elapsed().as_secs() < 10)
        {
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

        if let Some(client) = self.connection.client_mut()
            && save_data.locations.len() > self.locations_sent
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
            .connection
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
        let Some(client) = self.connection.client_mut() else {
            return Ok(());
        };
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
        if !locations.is_empty() {
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
        let Some(client) = self.connection.client_mut() else {
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
        let Some(client) = self.connection.client() else {
            return false;
        };

        client.slot_data().options.death_link != DeathLinkOption::Off
            && self.last_death_link.elapsed() >= DEATH_LINK_GRACE_PERIOD
    }

    /// Detects when the player has won the game and notifies the server.
    pub fn handle_goal(&mut self) -> Result<()> {
        if let Ok(event_man) = (unsafe { SprjEventFlagMan::instance() })
            && let Some(client) = self.connection.client_mut()
            && !self.sent_goal
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

    /// Writes a message to the log buffer that we display to the user in the
    /// overlay, as well as to the internal logger.
    pub fn log(&mut self, message: impl Into<ap::Print>) {
        let print = message.into();
        info!("[APC] {print}");
        // Consider making this a circular buffer if it ends up eating too much
        // memory over time.
        if self.log_buffer.len() >= LOG_BUFFER_LIMIT {
            self.log_buffer.pop_front();
        }
        self.log_buffer.push_back(print);
    }
}
