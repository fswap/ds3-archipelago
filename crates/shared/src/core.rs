use std::collections::VecDeque;
use std::time::{Duration, Instant};
use std::{io, iter::ExactSizeIterator, mem};

use anyhow::{Error, Result, bail};
use archipelago_rs as ap;
use log::*;
use serde::de::DeserializeOwned;

use crate::config::Config;

/// The maximum number of log messages to store.
///
/// This is relatively low because imgui is not very efficient about not
/// rendering the offscreen messages every frame, which can cause real slowdown
/// over long runs with chatty connections.
const LOG_BUFFER_LIMIT: usize = 200;

/// The grace period between MapItemMan starting to exist and the mod beginning
/// to take actions.
const GRACE_PERIOD: Duration = Duration::from_secs(10);

/// The base struct for implementations of [Core].
pub struct CoreBase<S: DeserializeOwned + Send + 'static> {
    /// The configuration for the current Archipelago connection. This is not
    /// guaranteed to be complete *or* accurate; it's the mod's responsibility
    /// to ensure it makes sense before actually interacting with an individual
    /// game.
    config: Config,

    /// The Archipelago client connection.
    connection: ap::Connection<S>,

    /// The log of prints displayed in the overlay.
    log_buffer: VecDeque<ap::Print>,

    /// Events we're waiting to process until the player loads a save. This is
    /// always empty unless a connection is connected and the player is on the
    /// main menu (or in the initial waiting period during a load).
    event_buffer: Vec<ap::Event>,

    /// The time at which we noticed the game loading (as indicated by
    /// MapItemMan coming into existence). Used to compute the grace period
    /// before we start doing stuff in game. None if the game is not currently
    /// loaded.
    load_time: Option<Instant>,

    /// The fatal error that this has encountered, if any. If this is not
    /// `None`, most in-game processing will be disabled.
    error: Option<Error>,
}

impl<S: DeserializeOwned + Send + 'static> CoreBase<S> {
    /// Creates a new instance of [CoreBase].
    pub fn new() -> Result<Self> {
        let config = Config::load()?;
        let connection = Self::new_connection(&config);
        Ok(Self {
            config,
            connection,
            log_buffer: Default::default(),
            event_buffer: vec![],
            load_time: None,
            error: None,
        })
    }

    /// Creates a new [ClientConnection] based on the connection information in [config].
    fn new_connection(config: &Config) -> ap::Connection<S> {
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
    pub(crate) fn connection_state_type(&self) -> ap::ConnectionStateType {
        self.connection.state_type()
    }

    /// Returns whether the current connection is disconnected.
    pub(crate) fn is_disconnected(&self) -> bool {
        self.connection.is_disconnected()
    }

    /// Retries the Archipelago connection with the same information.
    pub(crate) fn reconnect(&mut self) {
        if self.connection_state_type() == ap::ConnectionStateType::Disconnected {
            self.log("Reconnecting...");
        }

        self.connection = Self::new_connection(&self.config);
    }

    /// Updates the URL to use to connect to Archipelago and reconnects the
    /// Archipelago session.
    pub(crate) fn update_url(&mut self, url: impl AsRef<str>) -> Result<()> {
        if self.connection_state_type() == ap::ConnectionStateType::Disconnected {
            self.log("Reconnecting...");
        }

        self.config.set_url(url);
        self.config.save()?;
        self.connection = Self::new_connection(&self.config);
        Ok(())
    }

    /// If this client has encountered a fatal error, takes ownership of it.
    pub(crate) fn take_error(&mut self) -> Option<Error> {
        if let Some(err) = self.error.take() {
            self.error = Some(ap::Error::Elsewhere.into());
            Some(err)
        } else {
            None
        }
    }

    /// Returns the current user config.
    pub(crate) fn config(&self) -> &Config {
        &self.config
    }

    /// Returns the list of all logs that have been emitted in the current
    /// session.
    pub(crate) fn logs(&self) -> impl ExactSizeIterator<Item = &ap::Print> {
        self.log_buffer.iter()
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

    /// Writes a message to the log buffer that we display to the user in the
    /// overlay, as well as to the internal logger.
    fn log(&mut self, message: impl Into<ap::Print>) {
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

/// A trait for the core runners of FromSoftware game mods. This encapsulates
/// the interface that the shared overlay logic needs to interact with these
/// games.
pub trait Core: Send + Sized {
    /// The slot data for this runner.
    type SlotData: DeserializeOwned + Send + 'static;

    /// Creates a new instance of the mod.
    fn new() -> Result<Self>;

    /// Returns the base struct.
    fn base(&self) -> &CoreBase<Self::SlotData>;

    /// Returns the mutable base struct.
    fn base_mut(&mut self) -> &mut CoreBase<Self::SlotData>;

    /// Updates the game logic and checks for common errors. This is only run if
    /// we're currently connected to the Archipelago server and the mod has not
    /// encountered a fatal error.
    fn update_live(&mut self) -> Result<()>;

    /// Implementors may override this to handles custom command inputs via the
    /// say console. Returns whether a command was handled.
    ///
    /// By default, this doesn't handle any commands.
    fn handle_command(&mut self, _command: &str, _arg: Option<&str>) -> bool {
        false
    }

    /// Returns a reference to the Archipelago client, if it's connected.
    fn client(&self) -> Option<&ap::Client<Self::SlotData>> {
        self.base().connection.client()
    }

    /// Returns a mutable reference to the Archipelago client, if it's connected.
    fn client_mut(&mut self) -> Option<&mut ap::Client<Self::SlotData>> {
        self.base_mut().connection.client_mut()
    }

    /// Returns the seed the game expects to connect to.
    fn seed(&self) -> &str {
        self.base().config.seed()
    }

    /// Writes a message to the log buffer that we display to the user in the
    /// overlay, as well as to the internal logger.
    fn log(&mut self, message: impl Into<ap::Print>) {
        self.base_mut().log(message);
    }

    /// Consumes and returns all the as-yet-unprocessed events from the player's
    /// save.
    fn take_events(&mut self) -> Vec<ap::Event> {
        mem::take(&mut self.base_mut().event_buffer)
    }

    /// Runs the core logic of the mod. This may set [error], which should be
    /// surfaced to the user. Implementations should not override this; they
    /// should override [Self::update_live] instead.
    fn update(&mut self, is_main_menu: bool) {
        self.base_mut().update_always();

        if self.base().connection.client().is_none() || self.base().error.is_some() {
            return;
        }

        if is_main_menu {
            self.base_mut().load_time = None;
        } else if self.base().load_time.is_none() {
            self.base_mut().load_time = Some(Instant::now());
        }

        if let Some(time) = self.base().load_time
            && time.elapsed() < GRACE_PERIOD
        {
            return;
        }

        self.base_mut().error = match self.base().check_version_conflict() {
            Err(err) => Some(err),
            Ok(_) => self.update_live().err(),
        }
    }
}
