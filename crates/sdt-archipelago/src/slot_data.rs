use std::{collections::HashMap, hash::Hash, str::FromStr};

use sekiro::sprj::ItemId;
use serde::Deserialize;
use serde_repr::Deserialize_repr;

/// The slot data supplied by the Archipelago server which provides specific
/// information about how to set up this game.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlotData {
    /// A map from Archipelago's item IDs to DS3's.
    pub ap_ids_to_item_ids: HashMap<I64Key, DeserializableItemId>,

    /// A map from Archipelago's item IDs to the number of instances of that
    /// item the given ID should grant.
    pub item_counts: HashMap<I64Key, u32>,

    /// The options chosen by this player.
    pub options: Options,
}

#[derive(Debug, Deserialize)]
pub struct Options {
    /// Whether to kill the player when other players are killed and vice versa.
    pub death_link: DeathLinkOption,

    // New in 4.0
    /// How many deaths it takes to send a death link.
    #[serde(default = "default_death_link_amnesty")]
    pub death_link_amnesty: u8,
}

fn default_death_link_amnesty() -> u8 {
    1
}

/// Possible options for death link.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize_repr)]
#[repr(u8)]
pub enum DeathLinkOption {
    /// Death link is disabled.
    Off = 0,

    /// Death link triggers on any death.
    AnyDeath = 1,

    /// Death link only triggers for deaths when the player dies without
    /// collecting their last bloodstain.
    LostSouls = 2,
}

#[derive(Debug, Deserialize, Hash, PartialEq, Eq)]
#[serde(try_from = "&str")]
#[repr(transparent)]
pub struct I64Key(pub i64);

impl TryFrom<&str> for I64Key {
    type Error = <i64 as FromStr>::Err;

    fn try_from(value: &str) -> Result<I64Key, Self::Error> {
        Ok(I64Key(i64::from_str(value)?))
    }
}

/// A deserializable wrapper over [ItemId].
#[derive(Debug, Deserialize)]
#[serde(try_from = "u32")]
#[repr(transparent)]
pub struct DeserializableItemId(pub ItemId);

impl TryFrom<u32> for DeserializableItemId {
    type Error = <ItemId as TryFrom<u32>>::Error;

    fn try_from(value: u32) -> Result<DeserializableItemId, Self::Error> {
        Ok(DeserializableItemId(value.try_into()?))
    }
}
