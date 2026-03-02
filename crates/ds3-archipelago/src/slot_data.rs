use std::{collections::HashMap, hash::Hash, str::FromStr};

use darksouls3::sprj::{EventFlag, ItemId};
use serde::de::{Error, Unexpected};
use serde::{Deserialize, Deserializer};
use serde_repr::Deserialize_repr;

/// The slot data supplied by the Archipelago server which provides specific
/// information about how to set up this game.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlotData {
    /// Event flags that must all be set to true in order for the player to be
    /// considered to have achieved their goal.
    #[serde(default = "default_goal")]
    #[serde(deserialize_with = "deserialize_goal")]
    pub goal: Vec<EventFlag>,

    /// A map from Archipelago's item IDs to DS3's.
    pub ap_ids_to_item_ids: HashMap<I64Key, DeserializableItemId>,

    /// A map from Archipelago's item IDs to the number of instances of that
    /// item the given ID should grant.
    pub item_counts: HashMap<I64Key, u32>,

    /// The options chosen by this player.
    pub options: Options,
}

/// Deserializes a list of event flags, defaulting to the flag for defeating
/// Soul of Cinder.
fn deserialize_goal<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<EventFlag>, D::Error> {
    Vec::<u32>::deserialize(deserializer)?
        .into_iter()
        .map(|i| {
            EventFlag::try_from(i).map_err(|_| {
                D::Error::invalid_value(Unexpected::Unsigned(i.into()), &"a DS3 event flag")
            })
        })
        .collect()
}

/// The default goal, used because the DS3 AP 3.x world doesn't provide a list
/// of goal events.
fn default_goal() -> Vec<EventFlag> {
    vec![14100800.try_into().unwrap()]
}

#[derive(Debug, Deserialize)]
pub struct Options {
    /// Whether to kill the player when other players are killed and vice versa.
    pub death_link: DeathLinkOption,

    /// Whether the player's Archipelago expects the DS3 DLC to be enabled.
    #[serde(deserialize_with = "int_to_bool")]
    pub enable_dlc: bool,

    // New in 4.0
    /// How many deaths it takes to send a death link.
    #[serde(default = "default_death_link_amnesty")]
    pub death_link_amnesty: u8,
}

/// Deserializes an integer as a boolean value.
fn int_to_bool<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(u64::deserialize(deserializer)? != 0)
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
