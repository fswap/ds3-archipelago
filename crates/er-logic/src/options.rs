//! Tolerant slot_data option parsing, extracted from `net.rs` `build_slot_config`.
//!
//! The apworld ships booleans as ints (`enable_dlc: 1`), but a real JSON `true` must also work — a
//! strict typed deserialize would FAIL THE CONNECTION on the int form. These helpers accept either
//! and default to `false`, so a missing/garbage option is simply inert.

use serde_json::Value;

/// Read `options.<key>` as a bool, accepting JSON bool OR int (nonzero = true). Absent/garbage =>
/// false.
pub fn parse_bool_option(slot_data: &Value, key: &str) -> bool {
    slot_data
        .get("options")
        .and_then(|o| o.get(key))
        .map(|v| match v {
            Value::Bool(b) => *b,
            Value::Number(n) => n.as_i64().map(|i| i != 0).unwrap_or(false),
            _ => false,
        })
        .unwrap_or(false)
}

/// `options.enable_dlc` (int-or-bool).
pub fn parse_dlc(slot_data: &Value) -> bool {
    parse_bool_option(slot_data, "enable_dlc")
}

/// `options.death_link` (int-or-bool).
pub fn parse_death_link(slot_data: &Value) -> bool {
    parse_bool_option(slot_data, "death_link")
}

/// `options.no_equip_load` (int-or-bool). Same option name on both our apworld and Bedrock/fswap's.
pub fn parse_no_equip_load(slot_data: &Value) -> bool {
    parse_bool_option(slot_data, "no_equip_load")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accepts_int_and_bool_forms() {
        assert!(parse_dlc(&json!({ "options": { "enable_dlc": 1 } })));
        assert!(parse_dlc(&json!({ "options": { "enable_dlc": true } })));
        assert!(!parse_dlc(&json!({ "options": { "enable_dlc": 0 } })));
        assert!(!parse_dlc(&json!({ "options": { "enable_dlc": false } })));
    }

    #[test]
    fn absent_option_or_options_block_is_false() {
        assert!(!parse_dlc(&json!({ "options": {} })));
        assert!(!parse_dlc(&json!({ "seed": "abc" })));
    }

    #[test]
    fn death_link_parses_independently() {
        let sd = json!({ "options": { "enable_dlc": 0, "death_link": 1 } });
        assert!(!parse_dlc(&sd));
        assert!(parse_death_link(&sd));
    }

    #[test]
    fn garbage_value_is_inert_not_fatal() {
        assert!(!parse_bool_option(
            &json!({ "options": { "x": "yes" } }),
            "x"
        ));
        assert!(!parse_bool_option(
            &json!({ "options": { "x": [1, 2] } }),
            "x"
        ));
    }

    #[test]
    fn no_equip_load_parses() {
        assert!(parse_no_equip_load(
            &json!({ "options": { "no_equip_load": 1 } })
        ));
        assert!(parse_no_equip_load(
            &json!({ "options": { "no_equip_load": true } })
        ));
        assert!(!parse_no_equip_load(
            &json!({ "options": { "no_equip_load": 0 } })
        ));
        assert!(!parse_no_equip_load(&json!({ "options": {} })));
    }
}
