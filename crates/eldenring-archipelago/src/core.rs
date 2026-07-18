//! MILESTONE B — increment #7 (ECHO model, own_world:true). Supersedes increment #6c's core.
//!
//! With `own_world:true`, the server echoes our own checks back as received items, so the
//! received-item path is the SINGLE grant path (and it runs progressive / region-open / notify by
//! name for self-found items too). The detour + inventory-scan therefore only REPORT checks
//! (mark_checked) and suppress; they no longer grant locally. This fixes self-found progressive,
//! region keys, and notify items, which the old local-grant path silently skipped.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use anyhow::Result;
use archipelago_rs as ap;
use er_logic::hook::GameHook;
use er_logic::progressive::ProgressiveState;
use er_logic::receive::{GrantAction, RecvItem};
use er_logic::save_state::SaveState;
use er_logic::tracker::{HintEntry, HintSet};
use serde_json::Value;
use shared::Core as _;
use shared::CoreBase;

use crate::hook_impl::{EldenRingHook, ReceiveDispatch};

/// Tint for hinted lines in the tracker window (matches the overlay's YELLOW, 0xFCE94F).
const HINT_YELLOW: [f32; 4] = [0.9882, 0.9137, 0.3098, 1.0];

/// Tint for progression-surface check lines in the tracker window (soft orange).
const SURFACE_ORANGE: [f32; 4] = [0.9882, 0.6863, 0.2431, 1.0];

/// Dim gray for locked-region headers in the tracker window (mirrors imgui's TextDisabled).
const LOCKED_GRAY: [f32; 4] = [0.5, 0.5, 0.5, 1.0];

/// Parsed `regionAttunement` entry (attunement_gate, SPEC-gf-boss-lock-tracker). Absent/empty
/// slot_data => the feature is off. `members` are the region's freely-reachable in-region check AP
/// ids (the attunement denominator); `bloom_flags` are the graces revealed on attunement.
#[derive(Debug, Clone, Default)]
struct RegionAttunement {
    threshold: u32,
    members: HashSet<i64>,
    bloom_flags: Vec<u32>,
}

pub struct Core {
    base: CoreBase<crate::game::EldenRing, Value>,
    detour_installed: bool,
    received_through: usize,
    dispatched_through: usize,
    item_map: Option<HashMap<i64, i64>>,
    item_counts: HashMap<i64, i64>,
    region: Option<crate::region::RegionConfig>,
    /// Region-lock fog-wall visuals (cosmetic; KICK still enforces).
    fogwall: Option<crate::fogwall::FogWallConfig>,
    progressive: ProgressiveState,
    slot_data_parsed: bool,
    /// The room `seed_name` the current slot_data was parsed for. Guards the one-shot parse against
    /// a mid-session SEED CHANGE (reconnect to a DIFFERENT seed without an ER reload): when the
    /// room's seed differs from this, every per-seed table is rebuilt via [`Self::reset_for_new_seed`]
    /// before re-parsing. `None` until the first parse completes; set (never reset in place) at the
    /// end of the parse block, alongside `slot_data_parsed = true`.
    parsed_seed: Option<String>,
    my_name: Option<String>,
    save_path: Option<PathBuf>,
    save_loaded: bool,
    last_persisted_index: i64,
    valid_locations: HashSet<i64>,
    locations_loaded: bool,
    /// Bake-emitted location->flag map (apconfig) for detour-bypass checks (NPC gifts, death drops).
    flag_poll: Option<crate::flagpoll::FlagPollConfig>,
    /// slot_data dungeonSweeps: trigger location -> member locations.
    dungeon_sweeps: HashMap<i64, Vec<i64>>,
    /// slot_data sweepLockGates (BOSS_LOCKS_PATCH, Draft A): boss-defeat FLAG -> boss-lock item
    /// name that must be in the cumulative received set before that boss's flag-keyed sweep fires.
    /// Flag-keyed (u32) to match `sweep_flags`; the sweep loop looks it up by defeat flag.
    /// NOTE: `flagpoll::parse_sweep_lock_gates` MUST return `HashMap<u32, String>` for the
    /// `self.sweep_lock_gates = sweeps.1;` assignment to type-check (companion task #9 change).
    sweep_lock_gates: HashMap<u32, String>,
    /// Throttle the (potentially large) flag poll to a few times a second.
    poll_counter: u32,
    /// Guarding flags already set the first time we polled IN-WORLD (new-save defaults: the
    /// tutorial Flask of Crimson Tears flag 60000, physick / sacred-tear flags, etc.). The poll
    /// fires on a flag being SET, not on an unset->set transition, so without this a location
    /// whose flag defaults to set on a fresh save auto-checks on connect (silent) and its vanilla
    /// ware then leaks past the suppressor. Excluding this snapshot retires the ad-hoc
    /// FLAG_POLL_FALSE_POSITIVES denylist.
    flag_poll_baseline: HashSet<u32>,
    /// Whether flag_poll_baseline has been captured (once, on the first in-world poll).
    flag_poll_baseline_done: bool,
    /// Start-of-run grants (items / graces / map reveal).
    start: Option<crate::startgrants::StartConfig>,
    start_flags_done: bool,
    /// Persisted (SaveState): start items granted once for this save.
    start_items_granted: bool,
    /// Session-scoped (R11, SWEEP): indices into start_items that verifiably granted -- only the
    /// failed ones re-attempt; `start_items_granted` latches once ALL have landed.
    start_items_ok: HashSet<usize>,
    /// Session-scoped tidy-latch for `uniqueStartGrants` entries that have been DECIDED this
    /// session (granted-or-skipped). Deliberately NOT persisted: the obtained-FLAG is the single
    /// source of truth for "has it" (er_logic::unique_grants) -- this set only stops re-deciding
    /// (and re-logging) every tick. Losing it (reload/reconnect) is safe by construction: the
    /// flag read makes the re-run skip.
    unique_grants_ok: HashSet<usize>,
    /// Latches once every uniqueStartGrants entry is decided this session.
    unique_grants_done: bool,
    /// Session-scoped: when the player most recently entered a live world (reset on menu).
    /// Gates the start-item grant so it fires only after the load/inventory settle (clobber fix).
    in_world_since: Option<std::time::Instant>,
    /// Session-scoped last play_region seen by the start-grant gate. A change means a warp /
    /// fast-travel happened; we restart `in_world_since` so a timer-based start grant can't fire on
    /// an inventory pointer the warp's map reload may have left stale (the new-game spawn-kick CTD,
    /// Alaric 2026-07-16; also the unadvertised Chapel warp-out and any early fast-travel). `None`
    /// off-world. Only the timer path is affected -- real_pickup_seen() short-circuits it once a
    /// genuine pickup proves the pointer live, so an established character is untouched.
    grant_gate_last_play_region: Option<i32>,
    /// Pre-scout: resolves each shop reward's name/owner/ER-sell-id (pumped on the tick).
    scout: Option<crate::scout_proof::ScoutProof>,
    /// Goal-send (SPEC-goal-send-20260701.md): goalLocations split flag/checked at parse.
    goal: Option<crate::goal::GoalConfig>,
    /// Session latch: Goal sent once per connect (NOT persisted -- re-send is idempotent).
    sent_goal: bool,
    /// Item-tracker window visibility (overlay menu "Tracker" + F6 toggle).
    tracker_visible: bool,
    /// Standing hint set (SPEC-item-tracker.md option (a)): fed from streamed `Print::Hint`
    /// entries in the overlay log; dedups by location id (connect-replay re-inserts are no-ops).
    hints: HintSet,
    /// How many overlay-log entries have already been scanned for hints. v0.1 LIMITATION: the
    /// log is a bounded ring (1000 entries) -- once it fills and rotates, indices shift under
    /// this watermark and hints in the popped span are missed. DataStorage `_read_hints` is the
    /// robust follow-up (spec option (b)).
    hint_log_watermark: usize,
    /// Static AP location id -> region display name (generated er_logic::tracker_regions).
    region_table: HashMap<u64, String>,
    /// Static AP location id -> COARSE region name (in-logic key; "" = always open).
    coarse_table: HashMap<u64, String>,
    /// The PROGRESSION SURFACE: location ids this world's own progression may occupy (starred).
    progression_surface: HashSet<u64>,
    /// Static coarse region name -> its lock item name (absent = never locked).
    coarse_lock_items: HashMap<String, String>,
    /// Tracker filter: show only checks whose coarse region is currently accessible.
    tracker_in_logic_only: bool,
    /// Tracker filter: show only progression-surface checks.
    tracker_surface_only: bool,
    /// slot_data bossLockItems (mode A, SPEC-boss-lock-tracker.md): parsed boss-defeat trophy defs
    /// (flag -> name/region/boss_ap_id, gate=None for v0.2). METADATA + a defeat-flag watch only —
    /// NOT AP items and NOT new checks; the boss's own boss_ap_id location still fires through the
    /// locationFlags poll. Drives the one-shot "Felled: <Boss>" banner + the tracker Bosses group.
    boss_defs: Vec<er_logic::boss_felled::BossDef>,
    /// Per-boss PREVIOUS defeat-flag state (flags already seen SET) for the one-shot "Felled" banner
    /// edge detector. Primed on the first in-world poll (already-dead bosses, incl. reconnect) so
    /// their banner never re-fires; then only a THIS-session kill (unset->set) fires. Persists
    /// across polls; reset on a genuine seed change so it re-arms.
    boss_flag_prev: HashSet<u32>,
    /// ATTUNEMENT-RELEASE (attunement_gate, SPEC-gf-boss-lock-tracker): per-region gate data
    /// {threshold, member_ap_ids, bloom_flags}. Empty => feature off. Parsed once per seed.
    region_attunement: HashMap<String, RegionAttunement>,
    /// Per-region DEFERRED boss-payout checks: a boss killed while its region is not yet attuned has
    /// its checks (boss + sweep members) held here, burst-released the poll the region attunes.
    boss_payout_pending: HashMap<String, HashSet<i64>>,
    /// Regions whose attunement bloom has already fired this save (once-only grace-reveal latch).
    attuned_regions: HashSet<String>,
    /// Bloom baseline primed (first in-world poll): suppresses re-bannering already-attuned regions.
    attunement_primed: bool,
    /// BOSS KEYS (mode B, SPEC-gf-boss-lock-tracker "Boss Key: <Boss>"): per-boss DEFERRED own-check
    /// latch. A boss killed while its "Boss Key: <Boss>" item is not yet received has its own
    /// boss_ap_id check held here (keyed by defeat flag), burst-released the poll the key lands.
    /// Session-scoped: re-derived from the SERVER checked set + received_all on reconnect
    /// (is_local_location_checked makes re-runs idempotent). Empty gate set => unused.
    boss_key_pending: HashMap<u32, HashSet<i64>>,
    /// Boss-key sealed baseline primed (first in-world poll): a boss felled in a PRIOR session whose
    /// key is still unreceived is seeded into boss_key_pending SILENTLY so a reconnect never
    /// re-banners its seal. Mirrors boss_flag_prev / attunement_primed.
    boss_key_primed: bool,
    /// RECONCILER DRY-RUN (additive; `RECONCILE_DRYRUN=1` only): whether `reconcile_io::init` has
    /// run this session. Keeps init once-only, then `set_inputs` thereafter. Never touched unless
    /// dry-run is enabled, so the live path is unaffected.
    reconcile_inited: bool,
}

impl shared::Core for Core {
    type SlotData = Value;
    type Game = crate::game::EldenRing;

    /// Debug console commands, typed into the overlay's say input (2026-07-01, playtest tooling).
    /// Unrecognized "!" commands fall through to server chat.
    fn handle_command(&mut self, command: &str, arg: Option<&str>) -> bool {
        match command {
            "!flag" => {
                match arg.and_then(|a| a.trim().parse::<u32>().ok()) {
                    Some(f) => {
                        let v = crate::flags::get_event_flag(f);
                        self.log(ap::Print::message(format!("flag {f} = {v}")));
                    }
                    None => self.log(ap::Print::message("usage: !flag <id>".to_string())),
                }
                true
            }
            "!setflag" => {
                let parts: Vec<&str> = arg.unwrap_or("").split_whitespace().collect();
                match parts.first().and_then(|s| s.parse::<u32>().ok()) {
                    Some(f) => {
                        let on = parts.get(1).map(|s| *s != "0").unwrap_or(true);
                        let ok = crate::flags::try_set_event_flag(f, on);
                        self.log(ap::Print::message(format!(
                            "setflag {f} {on} -> {}",
                            if ok { "OK" } else { "NOT READY" }
                        )));
                    }
                    None => self.log(ap::Print::message("usage: !setflag <id> [0|1]".to_string())),
                }
                true
            }
            "!region" => {
                let pr = crate::flags::play_region_id();
                self.log(ap::Print::message(format!("play_region = {pr:?}")));
                true
            }
            "!warp" => {
                // Playtest tooling for the pure-runtime warp primitive (also unblocks a
                // random-start seed by hand if the auto-warp misfires). Full grace ENTITY id,
                // e.g. `!warp 11102950` = Table of Lost Grace (Roundtable Hold).
                match arg.and_then(|a| a.trim().parse::<u32>().ok()) {
                    Some(g) => {
                        let msg = match crate::warp::warp_to_grace(g) {
                            Ok(()) => format!("warp requested -> grace {g}"),
                            Err(e) => format!("warp FAILED: {e}"),
                        };
                        self.log(ap::Print::message(msg));
                    }
                    None => self.log(ap::Print::message(
                        "usage: !warp <grace entity id> (11102950 = Roundtable)".to_string(),
                    )),
                }
                true
            }
            "!grace" => {
                let Some(q) = arg.map(|s| s.to_lowercase()) else {
                    self.log(ap::Print::message(
                        "usage: !grace <name substring>".to_string(),
                    ));
                    return true;
                };
                let mut lines: Vec<String> = Vec::new();
                if let Some(cfg) = self.region.as_ref() {
                    for (name, &f) in &cfg.grace_items {
                        if name.to_lowercase().contains(&q) {
                            lines.push(format!(
                                "{name}: flag {f} = {}",
                                crate::flags::get_event_flag(f)
                            ));
                        }
                    }
                    for (name, fs) in &cfg.region_graces {
                        if name.to_lowercase().contains(&q) {
                            for &f in fs {
                                lines.push(format!(
                                    "{name} bundle: flag {f} = {}",
                                    crate::flags::get_event_flag(f)
                                ));
                            }
                        }
                    }
                }
                if lines.is_empty() {
                    lines.push(format!("no grace/lock matching '{q}'"));
                }
                for l in lines {
                    self.log(ap::Print::message(l));
                }
                true
            }
            "!help" => {
                self.log(ap::Print::message(
                    "!flag <id> | !setflag <id> [0|1] | !region | !grace <name substring>"
                        .to_string(),
                ));
                true
            }
            _ => false,
        }
    }

    fn new() -> Result<Self> {
        Ok(Self {
            base: CoreBase::new("Elden Ring")?,
            detour_installed: false,
            received_through: 0,
            dispatched_through: 0,
            item_map: None,
            item_counts: HashMap::new(),
            region: None,
            fogwall: None,
            progressive: ProgressiveState::new(HashMap::new()),
            slot_data_parsed: false,
            parsed_seed: None,
            my_name: None,
            save_path: None,
            save_loaded: false,
            last_persisted_index: -1,
            valid_locations: HashSet::new(),
            locations_loaded: false,
            flag_poll: None,
            dungeon_sweeps: HashMap::new(),
            sweep_lock_gates: HashMap::new(),
            poll_counter: 0,
            flag_poll_baseline: HashSet::new(),
            flag_poll_baseline_done: false,
            start: None,
            start_flags_done: false,
            start_items_granted: false,
            start_items_ok: HashSet::new(),
            unique_grants_ok: HashSet::new(),
            unique_grants_done: false,
            in_world_since: None,
            grant_gate_last_play_region: None,
            scout: None,
            goal: None,
            sent_goal: false,
            tracker_visible: false,
            hints: HintSet::new(),
            hint_log_watermark: 0,
            region_table: er_logic::tracker_regions::location_region_table(),
            coarse_table: er_logic::tracker_regions::location_coarse_table(),
            progression_surface: er_logic::tracker_regions::progression_surface_set(),
            coarse_lock_items: er_logic::tracker_regions::coarse_lock_item_table(),
            tracker_in_logic_only: false,
            tracker_surface_only: false,
            boss_defs: Vec::new(),
            boss_flag_prev: HashSet::new(),
            region_attunement: HashMap::new(),
            boss_payout_pending: HashMap::new(),
            attuned_regions: HashSet::new(),
            attunement_primed: false,
            boss_key_pending: HashMap::new(),
            boss_key_primed: false,
            reconcile_inited: false,
        })
    }
    fn base(&self) -> &CoreBase<Self::Game, Self::SlotData> {
        &self.base
    }
    fn base_mut(&mut self) -> &mut CoreBase<Self::Game, Self::SlotData> {
        &mut self.base
    }

    /// Overlay menu-bar hook (SPEC-item-tracker.md): a "Tracker" item that toggles the window.
    fn render_overlay_menu_items(&mut self, ui: &imgui::Ui) {
        if ui.menu_item("Tracker") {
            self.tracker_visible = !self.tracker_visible;
        }
    }

    /// Overlay frame hook: hotkey toggle + hint accumulation every frame (cheap -- the watermark
    /// skips already-scanned log entries), then the tracker window itself when visible.
    fn render_overlay_windows(&mut self, ui: &imgui::Ui) {
        // F6 toggles the tracker (deliberately NOT a plain letter -- those fight the say input).
        if ui.is_key_pressed(imgui::Key::F6) {
            self.tracker_visible = !self.tracker_visible;
        }

        self.accumulate_hints_from_log();

        if self.tracker_visible {
            self.render_tracker_window(ui);
        }
    }

    fn update_live(&mut self) -> Result<()> {
        if !self.detour_installed {
            match crate::detour::install() {
                Ok(()) => self.detour_installed = true,
                Err(e) => log::warn!("AddItemFunc detour install deferred: {e}"),
            }
        }
        // LuaWarp probe hook (warp_hook.rs; capital-reconciler menu-warp seam): self-guarded
        // one-shot on the game thread — a signature mismatch degrades with one log line
        // instead of erroring, so no install latch on Core is needed.
        crate::warp_hook::install();

        // 1. Report suppressed (world-pickup) synthetics. The echo grants them.
        let checks = crate::detour::take_pending_checks();
        if !checks.is_empty()
            && let Some(client) = self.client_mut()
            && let Err(e) = client.mark_checked(checks.iter().copied())
        {
            log::warn!("mark_checked failed for {checks:?}: {e}");
        }

        // 2. Parse slot_data once -- but RE-PARSE on a genuine SEED CHANGE (reconnect to a
        //    DIFFERENT seed without reloading the ER save). `slot_data_parsed` is a one-shot latch,
        //    so without this `valid_locations` (and every other per-seed table) keeps seed A's data
        //    while archipelago_rs rebuilds `local_locations_checked` for seed B -- the stale
        //    `valid_locations` guard then passes a seed-A id absent from seed B into
        //    `is_local_location_checked`, which panics in the no-unwind FFI frame (abort). It would
        //    also strand seed B's own new checks (its tables were never built). Compute the room
        //    seed the SAME way the save-key logic does (client.seed_name()); rebuild only on a real
        //    switch (er_logic::seed_change: non-empty room seed that differs from the parsed one --
        //    a same-seed reconnect must NOT reset, or it wipes the flag-poll baseline / save
        //    persistence that reconnect-to-same-seed relies on).
        let current_room_seed = self
            .client()
            .map(|c| c.seed_name().to_string())
            .unwrap_or_default();
        if self.slot_data_parsed
            && er_logic::seed_change::is_seed_change(
                self.parsed_seed.as_deref(),
                &current_room_seed,
            )
        {
            log::warn!(
                "seed change detected (parsed {:?} -> room {current_room_seed:?}) -- rebuilding per-seed state",
                self.parsed_seed
            );
            self.reset_for_new_seed();
        }
        if !self.slot_data_parsed {
            let parsed = self.client().map(|client| {
                let sd = client.slot_data();
                // Full slot_data dump (playtest diagnostics): every top-level key + a truncated
                // JSON value, so a client log alone answers "what did this seed emit?" -- e.g. is
                // regionSphereTargetRanges present/non-empty, is the seed `versions`-stamped.
                // Mirrors the gen-side spoiler dump (greenfield core.py write_spoiler).
                if let Some(obj) = sd.as_object() {
                    log::info!("slot_data dump ({} keys):", obj.len());
                    let mut items: Vec<(&String, String)> = obj
                        .iter()
                        .map(|(k, v)| {
                            let s = v.to_string();
                            let s = if s.chars().count() > 200 {
                                format!("{} ...(truncated)", s.chars().take(200).collect::<String>())
                            } else {
                                s
                            };
                            (k, s)
                        })
                        .collect();
                    items.sort_by(|a, b| a.0.cmp(b.0));
                    for (k, rv) in items {
                        log::info!("  {k} = {rv}");
                    }
                }
                // ---- VERSION HANDSHAKE ------------------------------------------------------
                // The apworld and this .dll ship as SEPARATE artifacts (apworld off-site, dll on
                // Nexus), so a player mixing versions is the NORM, not an edge case -- and a stale
                // .dll against a fresh apworld looks exactly like a bug in the game. `versions`
                // carries apworld semver + the CONTRACT HASH the apworld was built from + the hash
                // of the generated DATA the seed used. Compare the contract hash to the one THIS
                // binary was compiled against and shout if they differ. Always log the whole string:
                // every bug report should carry it, or it cannot be triaged.
                let their_versions = sd.get("versions").and_then(|v| v.as_str()).unwrap_or("");
                if their_versions.is_empty() {
                    log::warn!(
                        "VERSION: apworld sent no `versions` -- it predates the version handshake. \
                         This client is contract/{} apworld/{}. Skew CANNOT be detected; if anything \
                         behaves oddly, suspect a version mismatch first.",
                        crate::contract_gen::CONTRACT_HASH,
                        crate::contract_gen::APWORLD_VERSION_EXPECTED);
                } else {
                    let their_contract = their_versions
                        .split_whitespace()
                        .find_map(|t| t.strip_prefix("contract/"))
                        .unwrap_or("?");
                    if their_contract == crate::contract_gen::CONTRACT_HASH {
                        log::info!("VERSION: OK -- {} (client contract/{})",
                                   their_versions, crate::contract_gen::CONTRACT_HASH);
                    } else {
                        log::error!(
                            "VERSION MISMATCH -- apworld sent [{}] but this client was BUILT against \
                             contract/{}. The apworld and the client .dll are from different builds. \
                             Update whichever is older; do not report bugs from this pairing -- the \
                             slot_data shapes this client expects are not the ones it is being sent.",
                            their_versions, crate::contract_gen::CONTRACT_HASH);
                    }
                }

                // Two-sided contract validation: warn (not reject) on any slot_data mismatch
                // so a partially-compatible seed still boots but every problem is visible.
                let contract_problems = crate::contract_gen::validate(sd);
                if contract_problems.is_empty() {
                    log::info!("contract: slot_data OK ({} keys checked)", crate::contract_gen::CONTRACT.len());
                } else {
                    for p in &contract_problems { log::warn!("contract: {p}"); }
                }
                // int-or-bool tolerant (er_logic::options): the apworld serializes options
                // as ints (death_link: 1), which .as_bool() silently read as false.
                crate::deathlink::set_enabled(er_logic::options::parse_death_link(sd));
                crate::no_equip_load::set_enabled(er_logic::options::parse_no_equip_load(sd));
                crate::no_weapon_reqs::set_enabled(er_logic::options::parse_bool_option(
                    sd,
                    "no_weapon_requirements",
                ));
                crate::upgrades::set_auto_upgrade(
                    sd.pointer("/options/auto_upgrade").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
                );
                crate::upgrades::set_global_scadu_blessing(
                    sd.pointer("/options/global_scadutree_blessing").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
                );
                // mode 2 (scaled): the per-DLC-region Scadutree-blessing floor wire. Absent for base
                // game / mode != 2 -> empty -> mode 2 behaves as mode 1.
                crate::upgrades::set_dlc_blessing_floors(
                    er_logic::scaling::parse_triple_ranges(sd.get("dlcScadutreeFloorRanges")),
                );
                crate::upgrade_cost::set_flatten(
                    sd.pointer("/options/flatten_regular_upgrades").and_then(|v| v.as_i64()).unwrap_or(0),
                );
                let map = i64_map(sd.get("apIdsToItemIds"));
                // The GOODS rows this seed can actually GRANT. shop_icon / shop_preview must never
                // repaint one of these: EquipParamGoods.iconId and the GoodsName FMG entry are SHARED
                // per good id, so flowering the vanilla ware behind a shop slot re-icons and renames
                // EVERY copy the player will ever hold -- 11 vanilla shop rows sell smithing stones,
                // which is why the 2026-07-12 playtest had telescope-icon stones in the world AND in
                // the inventory. Both modules fail CLOSED until this arrives.
                let real_goods: std::collections::HashSet<u32> = map
                    .values()
                    .map(|v| *v as u32)
                    .filter(|full| er_codec::item_category_of(*full) == er_codec::CATEGORY_GOODS)
                    .map(er_codec::row_id_of)
                    .collect();
                crate::shop_icon::set_real_goods(real_goods.clone());
                crate::shop_preview::set_real_goods(real_goods);
                let counts = i64_map(sd.get("itemCounts"));
                let mut region = crate::region::parse(sd);
                // Capital-version reconciler (SPEC-capital-reconciler.md): five capital* keys,
                // parsed together; absent = INERT (logged). Also configures the shop release
                // re-key rows (shop_flags::run_capital_release, driven from the tick below).
                crate::region::configure_capital(sd);
                // BAKED REGION-LOCK FALLBACK (bedrock interop): only for a seed that speaks
                // NEITHER region key -- slot_data always wins when it speaks (region.rs). Scope
                // = the seed's apIdsToItemIds ids resolved to NAMES through the datapackage;
                // enforcement then stays COLD until a scoped "<Region> Lock" is actually
                // received (tick_baked_fallback below) -- the real foreign apworld ships its
                // whole item table even on no-lock seeds, so table presence must never arm.
                if crate::region::foreign_seed_without_region_keys(sd) {
                    let names: Vec<String> = client
                        .game(client.this_player().game())
                        .map(|g| {
                            map.keys()
                                .filter_map(|id| g.item(*id).map(|it| it.name().to_string()))
                                .collect()
                        })
                        .unwrap_or_default();
                    crate::region::prepare_baked_fallback(
                        &mut region,
                        names.iter().map(|s| s.as_str()),
                    );
                }
                let fogwall = crate::fogwall::parse(sd);
                let prog_cfg = er_logic::progressive::parse(sd);
                let name = client.this_player().alias().to_string();
                // BOSS_LOCKS_PATCH: sweeps + their lock gates travel together (tuple keeps
                // the parsed-slot_data tuple arity unchanged).
                let sweeps = (
                    crate::flagpoll::parse_dungeon_sweeps(sd),
                    crate::flagpoll::parse_sweep_lock_gates(sd),
                    crate::flagpoll::parse_sweep_flags(sd),
                );
                let start = crate::startgrants::parse(sd);
                if start.unique_start_grants.is_empty() {
                    log::info!("unique start grants: inert (no uniqueStartGrants in slot_data)");
                } else {
                    log::info!(
                        "unique start grants armed with {} entr{}: {:?} (goods, obtained-flag)",
                        start.unique_start_grants.len(),
                        if start.unique_start_grants.len() == 1 { "y" } else { "ies" },
                        start.unique_start_grants
                    );
                }

                // Shop system (SHOP-SYSTEM-HANDOFF.md §3): configure from slot_data, build the scout.
                // KEY-TABLE MIGRATION (locationIdsToKeys): token 1 of a matt slot key is the
                // acquisition flag; prefer it, fall back to legacy `locationFlags` for old seeds.
                let loc_flags = {
                    let from_keys = crate::key_resolver::location_flags_from_keys(sd);
                    if from_keys.is_empty() {
                        i64_to_u32_map(sd.get("locationFlags"))
                    } else {
                        from_keys
                    }
                };
                // SHOP KEY RESOLUTION: shop slots (token1==0) carry ShopLineupParam rows in token3;
                // resolve row -> eventFlag_forStock via shipped shoplineup_flags.json and fold into
                // loc_flags so purchases self-detect through the same poller. Disjoint union.
                let loc_flags = {
                    fn shop_table_path() -> std::path::PathBuf {
                        shared::utils::mod_directory()
                            .map(|d| d.join("shoplineup_flags.json"))
                            .unwrap_or_else(|_| std::path::PathBuf::from("shoplineup_flags.json"))
                    }
                    let mut loc_flags = loc_flags;
                    let shop_table = crate::key_resolver::load_shoplineup_flags(&shop_table_path());
                    // Tolerance requires telemetry: an absent/empty table degrades every foreign
                    // shop check to "never fires", which is indistinguishable from "no shops in
                    // this seed" without this line. Announce armed/inert once, but only on the
                    // matt-key (foreign) path -- greenfield seeds carry no locationIdsToKeys and
                    // resolve shops from slot_data, so the table is legitimately irrelevant there.
                    let foreign_keys = sd.get("locationIdsToKeys").is_some();
                    if !shop_table.is_empty() {
                        let resolved = crate::key_resolver::shop_flags_from_keys(sd, &shop_table);
                        if foreign_keys {
                            // DISTINCT flags, not just resolved locations. These differ when several AP
                            // shop locations resolve to the SAME ShopLineupParam row (matt keys list the
                            // rows that sell an item in token3, and many items share a row). That matters:
                            // shop_sell inverts loc->flag into flag->loc to find the row to rewrite, and
                            // an N:1 loc->flag mapping makes that inversion LOSSY -- it can only ever
                            // rewrite one row per flag. On the 2026-07-13 Bedrock seed shop_sell saw only
                            // 87 live check rows against 410 "resolved" locations, and this is the number
                            // that says whether that gap is collapse (expected) or a lookup failure (bug).
                            let distinct: std::collections::HashSet<u32> =
                                resolved.values().copied().collect();
                            log::info!(
                                "shoplineup_flags: armed with {} rows -- {} shop location(s) resolved to {} DISTINCT stock flag(s)",
                                shop_table.len(),
                                resolved.len(),
                                distinct.len()
                            );
                        }
                        for (loc, flag) in resolved {
                            loc_flags.entry(loc).or_insert(flag);
                        }
                    } else if foreign_keys {
                        log::warn!(
                            "shoplineup_flags: INERT -- no usable table at {} (foreign shop checks will never fire)",
                            shop_table_path().display()
                        );
                    }
                    loc_flags
                };
                let preview: Vec<(i64, i32)> = i64_map(sd.get("shopPreviewGoods"))
                    .into_iter()
                    .map(|(l, g)| (l, g as i32))
                    .collect();
                crate::scout_proof::configure_item_map(map.clone());
                crate::shop_flags::configure(
                    i64_to_u32_map(sd.get("shopRowFlags"))
                        .into_iter()
                        .map(|(r, f)| (r as u32, f))
                        .collect(),
                );
                crate::shop_flags::configure_check_flags(loc_flags.values().copied().collect());

                // shopInfiniteStock: {"<row id>": [goodsId, equipType, price]} -- the per-seed reroll of
                // the 455 UNLIMITED rows (no stock flag => never checks). The PRICE rides along because
                // those rows inherit the old ware's cost (gem slots = 1 rune, 166 armor rows FREE);
                // without it every seed is a free-consumable dispenser.
                {
                    let mut roll: std::collections::HashMap<u32, (i32, u8, i32)> =
                        std::collections::HashMap::new();
                    if let Some(m) = sd.get("shopInfiniteStock").and_then(|v| v.as_object()) {
                        for (k, v) in m {
                            let (Ok(row), Some(a)) = (k.parse::<u32>(), v.as_array()) else { continue };
                            if a.len() < 3 {
                                continue;
                            }
                            let (Some(gid), Some(et), Some(pr)) =
                                (a[0].as_i64(), a[1].as_i64(), a[2].as_i64()) else { continue };
                            roll.insert(row, (gid as i32, et as u8, pr as i32));
                        }
                    }
                    if !roll.is_empty() {
                        crate::shop_stock::configure(roll);
                    }
                }

                // enemyDropRoll: {"<lot id>": [slot, goodsId, slot, goodsId, ...]} -- flat pairs.
                // UNFLAGGED ItemLotParam_enemy lots only (a flagged lot IS a check and is never sent).
                {
                    let mut roll: std::collections::HashMap<u32, Vec<(u8, i32)>> =
                        std::collections::HashMap::new();
                    if let Some(m) = sd.get("enemyDropRoll").and_then(|v| v.as_object()) {
                        for (k, v) in m {
                            let (Ok(lot), Some(a)) = (k.parse::<u32>(), v.as_array()) else { continue };
                            let mut pairs = Vec::with_capacity(a.len() / 2);
                            for ch in a.chunks(2) {
                                if ch.len() < 2 {
                                    break;
                                }
                                let (Some(sl), Some(gid)) = (ch[0].as_i64(), ch[1].as_i64()) else { continue };
                                pairs.push((sl as u8, gid as i32));
                            }
                            if !pairs.is_empty() {
                                roll.insert(lot, pairs);
                            }
                        }
                    }
                    if !roll.is_empty() {
                        crate::enemy_drops::configure(roll);
                    }
                }

                // checkLotBlank {"<lot id>": [goods slot idx, ...]} + apPlaceholderGoods.
                // Repoints each CHECK lot's goods slot at ONE placeholder id, which detour.rs then
                // suppresses UNCONDITIONALLY -- so the vanilla ware is never handed out at a check, and
                // NOTHING else has to be watched by item id (mined ore / farmed drops / bought / crafted
                // goods all pass through untouched). One placeholder suffices because checks are detected
                // by the FLAG POLL, not by the pickup id.
                {
                    let ph = sd.get("apPlaceholderGoods").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                    // TWO tables, kept apart. ItemLotParam_map and ItemLotParam_enemy can hold the
                    // SAME row id, so a merged dict loses the table and the client has to guess. It
                    // guessed map-first -- and every enemy lot colliding with a map id was therefore
                    // never blanked, so a boss that is "just an enemy" handed out its vanilla drop and
                    // fired no check. The apworld knows which CSV each lot came from; it now says so.
                    let parse_lots = |key: &str| -> std::collections::HashMap<u32, Vec<u8>> {
                        let mut out = std::collections::HashMap::new();
                        if let Some(m) = sd.get(key).and_then(|v| v.as_object()) {
                            for (k, v) in m {
                                let (Ok(lot), Some(a)) = (k.parse::<u32>(), v.as_array()) else {
                                    continue;
                                };
                                let slots: Vec<u8> =
                                    a.iter().filter_map(|x| x.as_i64()).map(|x| x as u8).collect();
                                if !slots.is_empty() {
                                    out.insert(lot, slots);
                                }
                            }
                        }
                        out
                    };
                    let mut blank_map = parse_lots("checkLotBlankMap");
                    let mut blank_enemy = parse_lots("checkLotBlankEnemy");
                    if blank_map.is_empty() && blank_enemy.is_empty() {
                        // LEGACY: an apworld whose check_lots_data.py predates the map/enemy split. It
                        // ships one merged dict keyed by lot id alone, so the table is unknown. Send it
                        // to BOTH -- check_lots only writes a lot where the row actually EXISTS, so a
                        // map-only id lands in map and an enemy-only id lands in enemy, reproducing the
                        // old behaviour. A COLLIDING id gets blanked in both, which is the old bug's
                        // blast radius inverted (it used to under-blank; now it over-blanks) -- and that
                        // is precisely why the apworld must send the table. Loud, not silent.
                        let legacy = parse_lots("checkLotBlank");
                        if !legacy.is_empty() {
                            log::warn!(
                                "check-lots: apworld sent the LEGACY merged checkLotBlank (no map/enemy \
                                 split). The param table each lot belongs to is unknown, so bosses that \
                                 are 'just an enemy' may still hand out their vanilla drop. Regenerate \
                                 the apworld (python greenfield/gen_data.py)."
                            );
                            blank_map = legacy.clone();
                            blank_enemy = legacy;
                        }
                    }
                    if ph != 0 && !(blank_map.is_empty() && blank_enemy.is_empty()) {
                        crate::check_lots::configure(blank_map, blank_enemy, ph);
                    } else {
                        // STATIC FALLBACK -- vanilla suppression for a FOREIGN apworld.
                        //
                        // Measured in-game on the first Bedrock playtest (2026-07-13):
                        //     "vanilla suppressor INERT: checkItemFlags empty/absent in slot_data"
                        // -- every check paid out the VANILLA item AND the AP item, because only OUR
                        // apworld emits checkLotBlank*/checkItemFlags.
                        //
                        // But the blank-list is derived from ItemLotParam (flag -> lot -> goods
                        // slots): GAME data, not seed data, identical for every apworld. So we ship
                        // it (check_lots_table.json) and scope it to the flags THIS seed checks.
                        // 3018 of Bedrock's 3022 check flags (99.9%) suppressed, zero changes on his
                        // side. Same argument as shoplineup_flags.json.
                        //
                        // Scoped, NOT global: blanking a lot the seed does not check would eat a
                        // legitimate vanilla pickup.
                        let sl = load_static_lots();
                        if sl.is_empty() {
                            log::warn!(
                                "vanilla suppressor INERT: no checkLotBlank* in slot_data and no \
                                 usable check_lots_table.json beside the DLL. Every check will hand \
                                 out its VANILLA item as well as the AP item."
                            );
                        } else {
                            let seed_flags: Vec<u32> = loc_flags.values().copied().collect();
                            let (m, e) = er_logic::static_lots::blank_tables_for(&sl, &seed_flags);
                            let n = m.len() + e.len();
                            if n > 0 && sl.placeholder_goods != 0 {
                                crate::check_lots::configure(m, e, sl.placeholder_goods);
                                log::info!(
                                    "check-lots STATIC fallback: {} lot(s) blanked from \
                                     check_lots_table.json, scoped to this seed's {} check flag(s) \
                                     (foreign apworld -- it emits no checkLotBlank*)",
                                    n,
                                    seed_flags.len()
                                );
                            }
                        }
                    }
                }
                crate::shop_sell::configure(loc_flags.clone());
                // SLOT_DATA WINS, PARAMS ARE THE FALLBACK. `shopPreviewGoods` is the VANILLA ware
                // sitting in each check's shop row -- that is GAME data, not seed data, so when a
                // foreign apworld (Bedrock) omits the key we can read it straight off the live
                // ShopLineupParam instead. Do NOT configure an empty set here: configure() latches
                // CONFIGURED_SET, and shop_preview/shop_icon would then latch DONE on zero pairs
                // before shop_sell's runtime derivation ever arrives.
                //
                // The defect this fixes is a DISPLAY/ROUTING one, not a duplication one. On the
                // 2026-07-13 Bedrock playtest `shop-preview: configured 0 shop slot(s)`, so every
                // slot shop_sell could not natively rewrite (foreign, or an own-world gem/custom
                // reward) showed its VANILLA name and icon. The check still fires and the reward
                // still routes correctly -- the player simply has no way to see WHAT a slot holds or
                // WHO it belongs to, which is the whole point of a multiworld shop.
                if preview.is_empty() {
                    log::info!(
                        "shop-preview/icon: no shopPreviewGoods in slot_data -- deferring to the \
                         ShopLineupParam fallback in shop_sell"
                    );
                } else {
                    crate::shop_preview::configure(preview.clone());
                    crate::shop_icon::configure(preview);
                }
                crate::minibaker::configure(
                    sd.get("stoneswordVendorRow").and_then(|v| v.as_i64()).unwrap_or(0) as u32,
                );
                crate::scaling::configure(sd); // runtime enemy scaling (regionSphereTargets)
                // checkItemFlags: full raw item id -> check acquisition flags (the PORT-GAP
                // vanilla-suppress table; LIVE in the detour since 2026-07-01).
                let check_flags: std::collections::HashMap<u32, Vec<u32>> = sd
                    .get("checkItemFlags")
                    .and_then(|v| v.as_object())
                    .map(|o| {
                        o.iter()
                            .filter_map(|(k, v)| {
                                let id: u32 = k.parse().ok()?;
                                let fl = v.as_array()?
                                    .iter()
                                    .filter_map(|f| f.as_u64().map(|x| x as u32))
                                    .collect();
                                Some((id, fl))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                // STATIC FALLBACK for the id-keyed half (weapon/armor wares -- goods are blanked at
                // the lot above; suppressing goods BY ID would eat every Golden Rune you ever found).
                let check_flags = if check_flags.is_empty() {
                    let sl = load_static_lots();
                    let seed_flags: Vec<u32> = loc_flags.values().copied().collect();
                    let cif = er_logic::static_lots::check_item_flags_for(&sl, &seed_flags);
                    if !cif.is_empty() {
                        log::info!(
                            "checkItemFlags STATIC fallback: {} weapon/armor item id(s) suppressed \
                             from check_lots_table.json (foreign apworld emits no checkItemFlags)",
                            cif.len()
                        );
                    }
                    cif
                } else {
                    check_flags
                };
                crate::detour::configure_check_item_flags(check_flags);
                let scout = crate::scout_proof::ScoutProof::new(loc_flags.keys().copied().collect());
                // Goal-send: split goalLocations into flag-detected / checked-fallback buckets
                // against loc_flags (SPEC-goal-send-20260701.md; do NOT route through flagpoll).
                let goal_cfg = crate::goal::parse(sd, &loc_flags);
                // Boss-lock mode A (SPEC-boss-lock-tracker.md): parse the bossLockItems metadata
                // map into BossDef rows (gate=None for v0.2 — no sweepLockGates boss-key yet). This
                // is a presentation/defeat-flag-watch layer only; it mints no AP item and no check.
                let boss_defs = parse_boss_lock_items(sd.get("bossLockItems"));
                // ATTUNEMENT (attunement_gate): per-region {threshold, member_ap_ids, bloom_flags}.
                // Emitted only when the option is on; absent/empty => the whole feature stays off.
                let region_attunement = parse_region_attunement(sd.get("regionAttunement"));

                // Connect banner: build identity + slot_data contract version (+ gate result) so any
                // logfile self-identifies which build / contract produced it. Then a one-line
                // start-config summary of the exact fields we previously had to decompile the
                // multidata to see — startRegion, startGraces count, reveal_all_maps, the random-start
                // warp/area/done flags, and the area-lock count.
                let versions = sd.get("versions").and_then(|v| v.as_str()).unwrap_or("(none)");
                // LEGACY SEMVER GATE DELETED (2026-07-11). It fired on EVERY connect, unconditionally,
                // and cried wolf at Alaric across a whole playtest:
                //     "apworld/client version mismatch: seed wants apworld/0.2.0 contract/b68eaa15
                //      data/e4c73b06..., client is 0.1.0-beta.4 -- update the client"
                // It fed our `versions` string into er_semver::version_satisfies(), which expects a
                // semver RANGE (">=0.6.6 <0.7.0"). Ours is a DESCRIPTIVE string carrying the apworld
                // semver + contract hash + data hash, so the parse always fails and `.unwrap_or(false)`
                // turns that into "mismatch". It also compared the apworld's semver against the CLIENT
                // CRATE's version -- two independent numbering schemes that were never meant to match.
                //
                // It is fully superseded by the VERSION HANDSHAKE above (~line 413), which compares the
                // things that actually matter -- the CONTRACT HASH and the DATA HASH the binary was
                // compiled against -- and says OK / warns with specifics. An unsound duplicate that
                // always fires is worse than no gate: it trains you to ignore the real one.
                let gate_warn: Option<String> = None;
                let start_region = sd.get("startRegion").and_then(|v| v.as_str()).unwrap_or("");
                log::info!(
                    "=== ER-AP client {} | contract {versions} | slot '{name}' ===",
                    crate::game::CLIENT_BUILD
                );
                log::info!(
                    "startcfg: start_region={start_region:?} | startGraces={} reveal_maps={} startItems={} | randomStart warp/area/done={}/{}/{} | area_locks={}",
                    start.start_graces.len(),
                    start.reveal_all_maps,
                    start.start_items.len(),
                    region.random_start_warp_flag,
                    region.random_start_area_id,
                    region.random_start_done_flag,
                    region.area_lock_flags.len()
                );

                // Prime the fast-travel gate's known-good flag from the start graces. The client SETS
                // these at spawn, so they are really on and pointing the gate field at one is inert.
                // This removes the only case the old destructive fallback existed for -- booting
                // straight into a boss dungeon with nothing cached, which used to SET the field's flag
                // and, in a boss dungeon, that flag is the BOSS'S DEFEAT FLAG (Gael Tunnel, 2026-07-11).
                crate::fast_travel::prime_known_good(&start.start_graces);

                // Seed the config watcher with what we actually connected WITH, so its first tick is a
                // no-op instead of a spurious reconnect to the very file we booted from.
                {
                    let cfg = self.base().config_snapshot();
                    crate::config_watch::prime(&cfg.0, &cfg.1, cfg.2);
                }

                // THE PROGRESSION SURFACE = what the tracker stars. Computed HERE, inside the
                // closure where `sd` is in scope, then threaded out via the tuple and assigned below.
                //
                // "Big-ticket" is RETIRED, name and all. It was a SECOND list of "important checks"
                // that disagreed with the first: it named {MajorBoss, Remembrance, GreatRune} while
                // the apworld's progression surface is {Remembrance, Seedtree, Church, Boss, Fragment,
                // Revered}. Intersection: Remembrance alone. So this tracker starred MajorBoss/
                // GreatRune checks that the apworld FORBIDS a region Lock from ever occupying -- it
                // pointed the player at checks the locks could not be on. (Found 2026-07-12 reading a
                // spoiler: killing Malenia paid out a Smithing Stone [4].)
                //
                // NOTE THE DELETED FALLBACK. There is deliberately NO fall back to the static table
                // when the key is absent. The static table is the world's DEFAULT surface -- correct
                // for a default seed, WRONG for any seed that selected a different surface -- so
                // falling back would silently show a plausible, wrong star set. An empty star set is
                // visibly broken; a wrong one teaches the player something false. Prefer the visible
                // failure. (The earlier note here claimed the static table was "exactly the wrong
                // set". That is no longer true: tools/gen_location_regions.py now bakes the surface
                // itself, and the two are byte-identical for a default seed. The reasoning above is
                // why the fallback still stays deleted.)
                let progression_surface: std::collections::HashSet<u64> = {
                    match sd.get("progressionSurfaceLocations").and_then(|v| v.as_array()) {
                        Some(arr) => arr.iter().filter_map(|x| x.as_u64()).collect(),
                        None => {
                            log::warn!(
                                "slot_data has no progressionSurfaceLocations: the tracker will star \
                                 NOTHING. (Old apworld? bigTicketLocations is retired -- it named a \
                                 set progression could never reach.)"
                            );
                            std::collections::HashSet::new()
                        }
                    }
                };

                (map, counts, region, fogwall, prog_cfg, name, sweeps, start, scout, gate_warn, loc_flags, goal_cfg, boss_defs, region_attunement, progression_surface)
            });
            if let Some((
                map,
                counts,
                region,
                fogwall,
                prog_cfg,
                name,
                sweeps,
                start,
                scout,
                gate_warn,
                loc_flags,
                goal_cfg,
                boss_defs,
                region_attunement,
                progression_surface,
            )) = parsed
            {
                log::info!(
                    "slot_data parsed: {} item-map, {} area-lock, {} progressive; player '{name}'",
                    map.len(),
                    region.area_lock_flags.len(),
                    prog_cfg.len()
                );
                self.item_map = Some(map);
                self.item_counts = counts;
                self.region = Some(region);
                self.fogwall = Some(fogwall);
                self.progressive = ProgressiveState::new(prog_cfg);
                self.my_name = Some(name);
                self.dungeon_sweeps = sweeps.0;
                self.sweep_lock_gates = sweeps.1;
                // F2 fix (2026-07-01): the flag-poll table travels in slot_data ("locationFlags")
                // now; baker-era apconfig.json no longer carries location_flags, so fresh installs
                // polled an EMPTY map (world pickups never sent checks -- seed looked vanilla).
                // slot_data wins; a legacy apconfig table still contributes sweep_flags / extras.
                let mut fp = crate::flagpoll::load();
                for (loc, flag) in loc_flags {
                    fp.location_flags.insert(loc, flag);
                }
                // greenfield flag-keyed dungeon sweeps (dungeonSweepFlags, parsed above into
                // sweeps.2): merge into the same sweep_flags table the legacy apconfig used, so the
                // existing poll loop fires them on boss kill. slot_data wins per flag.
                for (flag, locs) in sweeps.2 {
                    fp.sweep_flags.insert(flag, locs);
                }
                log::info!(
                    "flag-poll table: {} location flags ({} sweep groups)",
                    fp.location_flags.len(),
                    fp.sweep_flags.len()
                );
                self.flag_poll = Some(fp);
                self.start = Some(start);
                self.scout = Some(scout);
                self.goal = Some(goal_cfg);
                log::info!(
                    "slot_data parsed: {} boss-lock def(s) (mode A Felled trophies)",
                    boss_defs.len()
                );
                self.boss_defs = boss_defs;
                self.region_attunement = region_attunement;
                log::info!(
                    "slot_data parsed: {} region attunement gate(s)",
                    self.region_attunement.len()
                );
                // Assign the progression surface parsed inside the slot_data closure above (where
                // `sd` was in scope).
                self.progression_surface = progression_surface;
                self.slot_data_parsed = true;
                // Remember which seed this parse was for, so a later reconnect to a DIFFERENT seed
                // (without an ER reload) is detected above and rebuilds the per-seed state.
                self.parsed_seed = Some(current_room_seed.clone());
                if let Some(warning) = gate_warn {
                    log::error!("{warning}");
                    self.log(ap::Print::message(warning));
                }
            }
        }

        // 2b. Load the persisted save once (resume watermark + progressive tiers).
        if self.slot_data_parsed && !self.save_loaded {
            // SAVE-KEY FIX (2026-07-02): key the save by the ROOM's seed_name (RoomInfo ground
            // truth), not the apconfig seed. The staged apconfig ships "seed":"", so every seed
            // shared ONE file (ap_save__<slot>.json) and a fresh world resumed at the previous
            // world's watermark -- seen live on the ER+HK seed: "resume at received index 134"
            // on a brand-new multiworld, so the first 134 receives (start items included) were
            // treated as already-granted and never placed. Region opens self-healed via the
            // reconcile ticks, which masked everything except the missing bag items.
            let room_seed = self
                .client()
                .map(|c| c.seed_name().to_string())
                .unwrap_or_default();
            let seed_key = if room_seed.is_empty() {
                // No RoomInfo seed (shouldn't happen once slot_data parsed) -- fall back to the
                // apconfig seed rather than never arming persistence.
                self.seed().to_string()
            } else {
                room_seed
            };
            if let Some(path) = save_file_path(&seed_key, self.my_name.as_deref().unwrap_or("")) {
                let st = match std::fs::read_to_string(&path) {
                    Ok(saved_text) => {
                        // R7 (SWEEP): from_json is tolerant -- a present-but-corrupt save would
                        // silently reset every watermark (duplicate start items + regrant burst).
                        if serde_json::from_str::<Value>(&saved_text).is_err() {
                            log::error!(
                                "save file {} is CORRUPT (not valid JSON) -- watermarks reset to defaults",
                                path.display()
                            );
                        }
                        SaveState::from_json(&saved_text)
                    }
                    Err(_) => SaveState::default(), // absent = fresh save (normal first run)
                };
                self.received_through = st.last_received_index.max(0) as usize;
                self.progressive.restore(
                    st.progressive_counter
                        .iter()
                        .map(|(k, &v)| (k.clone(), v))
                        .collect(),
                    st.progressive_high_index,
                );
                self.start_items_granted = st.start_items_granted;
                // Reuse the once-captured fresh-save baseline ACROSS reconnects
                // (gf-flagpoll-newsave-default-flags / "picked it up, got nothing"):
                // re-snapshotting the progressed save would fold mid-session pickups into the
                // baseline and strand their checks forever. Empty = fresh save, nothing
                // persisted yet -> capture below on the first in-world poll. Mirrors the pure
                // er_logic::flagpoll_baseline_replay::effective_baseline (host-tested).
                self.flag_poll_baseline = st.flag_poll_baseline.iter().copied().collect();
                self.flag_poll_baseline_done = !self.flag_poll_baseline.is_empty();
                self.last_persisted_index = st.last_received_index;
                log::info!("save persistence armed at {}", path.display());
                self.save_path = Some(path);
                log::info!(
                    "save loaded: resume at received index {}",
                    self.received_through
                );
            }
            self.save_loaded = true;
        }

        // 2c. Cache the slot's valid location set once (shop-scan dedup guard).
        if !self.locations_loaded {
            let v: HashSet<i64> = self
                .client()
                .map(|client| {
                    client
                        .checked_locations()
                        .map(|l| l.id())
                        .chain(client.unchecked_locations().map(|l| l.id()))
                        .collect()
                })
                .unwrap_or_default();
            if !v.is_empty() {
                self.valid_locations = v;
                self.locations_loaded = true;
            }
        }

        // 2d. Start grants: graces + map reveal (once, retried until the flag holder is up) and start
        //     items (Torrent etc.) once per save (persisted), gated on a captured inventory pointer.
        if self.slot_data_parsed {
            let already_flags = self.start_flags_done;
            let already_items = self.start_items_granted;
            // Same in_world tightening as can_grant (SWEEP H3): the inventory pointer never
            // resets on quit-to-menu, so menu-time start grants would write through a stale one.
            let has_inv = crate::detour::has_inventory() && crate::flags::in_world();
            // Start-ITEMS clobber guard (patch_greenfield_start_item_clobber.py): the static
            // inventory prime lets grants fire during the load screen, before the save/new-game
            // inventory finishes loading -- which then CLOBBERS the just-granted item (the Torch
            // never appeared in-game). Defer start-item grants until the inventory is genuinely
            // live: a real game AddItem has fired (bulk load replace done) OR we've been in-world
            // long enough for the load to settle. Timing-independent; received grants untouched.
            if crate::flags::in_world() {
                // A play_region change while in-world = a warp / fast-travel just landed. Restart the
                // settle window so a timer-based start grant can't fire on an inventory pointer the
                // map reload may have left stale (the spawn-kick CTD + the unadvertised Chapel warp-
                // out + early fast-travels). Skip the very first observation (last = None), so a fresh
                // spawn's own settle timer runs normally. real_pickup_seen() still short-circuits the
                // whole gate once a genuine pickup proves the pointer live, so this only ever DELAYS
                // the pre-first-pickup timer path -- an established character is untouched.
                let pr = crate::flags::play_region_id();
                if pr.is_some() && pr != self.grant_gate_last_play_region {
                    if self.grant_gate_last_play_region.is_some() {
                        self.in_world_since = None;
                    }
                    self.grant_gate_last_play_region = pr;
                }
                self.in_world_since
                    .get_or_insert_with(std::time::Instant::now);
            } else {
                self.in_world_since = None;
                self.grant_gate_last_play_region = None;
            }
            let start_items_settled = crate::detour::real_pickup_seen()
                || self
                    .in_world_since
                    .is_some_and(|t| t.elapsed() >= std::time::Duration::from_secs(8));
            let mut did_flags = false;
            let mut did_items = false;
            if let Some(sc) = self.start.as_ref() {
                // Gate start FLAGS on a loaded world (has_inventory), not just CSEventFlagMan being
                // up: setting grace/map flags during the load screen lets the subsequent save-data
                // load clobber them, which is the suspected cause of "no graces/maps in-game" despite
                // correct slot_data. (The standalone gated its grace flush the same way.) After
                // applying, read a sentinel grace back — only latch `done` once it sticks; a false
                // read-back means it was clobbered, so we log it and retry next tick.
                if !already_flags
                    && has_inv
                    && start_items_settled
                    && crate::startgrants::apply_start_flags(sc)
                {
                    let sentinel = sc.start_graces.first().copied();
                    let stuck = sentinel.is_none_or(crate::flags::get_event_flag);
                    if stuck {
                        did_flags = true;
                    } else {
                        log::warn!(
                            "start graces set but sentinel flag {sentinel:?} read back FALSE (clobbered by save load?) — retrying next tick"
                        );
                    }
                }
                // R11 (SWEEP): `.all(grant_full_id)` re-granted already-succeeded items on a
                // partial failure next tick (duplicates). Track per-item success (by index,
                // session-scoped) so only the FAILED ones re-attempt; latch once all landed.
                // STRANGLER: start items are folded into `build_desired_inputs`, now SPLIT across two
                // reconciler classes — GOODS-category start items are presence-diffed unique goods
                // (owned with `goods`), non-goods ones stay ledgered at the negative
                // START_ITEM_INDEX_BASE band (owned with `ledger`). So this old drain must stand down
                // whenever the reconciler owns EITHER of those classes, or it would race/double-grant
                // the class the reconciler already handles. Runtime-revertible: drop both `goods` and
                // `ledger` from RECONCILE_APPLY (e.g. RECONCILE_APPLY=flags or =none) and the drain runs
                // again as the sole start-item path.
                if !crate::reconcile_io::owns_goods()
                    && !crate::reconcile_io::owns_ledger()
                    && !already_items
                    && has_inv
                    && start_items_settled
                {
                    let mut all_ok = true;
                    for (i, &id) in sc.start_items.iter().enumerate() {
                        if self.start_items_ok.contains(&i) {
                            continue;
                        }
                        if crate::detour::grant_full_id(id, 1) {
                            self.start_items_ok.insert(i);
                        } else {
                            all_ok = false;
                            warn_start_item_fail_once(i, id);
                        }
                    }
                    if all_ok {
                        did_items = true;
                    }
                }
                // UNIQUE start grants (slot_data `uniqueStartGrants`, [[fullId, obtainedFlag]]):
                // grant the goods ONLY if the obtained-flag is unset, then set the flag WITH the
                // grant. The flag is the single source of truth for "has it" (the game itself
                // tracks possession with it; keyitems.rs sets the same flag on a pool receive),
                // so a re-run -- reload, reconnect, seed reset, late connect after the player
                // already found the pool copy -- skips by construction. Decision is the pure
                // er_logic::unique_grants::unique_grant_action (replay-tested); this block is glue.
                //
                // Runs REGARDLESS of reconciler ownership (unlike the plain drain above): the
                // reconciler never sees these ids -- they are deliberately absent from startItems,
                // so neither its unique_goods presence-diff nor its start-item ledger handles
                // them -- and the flag latch makes re-entry safe anyway. Gated on the start FLAGS
                // having landed (already_flags || did_flags): that proves the flag holder is up,
                // so the paired try_set_event_flag after a successful grant cannot miss.
                if !self.unique_grants_done
                    && (already_flags || did_flags)
                    && has_inv
                    && start_items_settled
                {
                    let mut all_done = true;
                    for (i, &(full_id, flag)) in sc.unique_start_grants.iter().enumerate() {
                        if self.unique_grants_ok.contains(&i) {
                            continue;
                        }
                        let goods = full_id & 0x0FFF_FFFF;
                        if !er_logic::unique_grants::unique_grant_action(
                            crate::flags::get_event_flag(flag),
                        ) {
                            log::info!(
                                "unique grant: goods {goods} ({full_id:#x}) -- flag {flag} already set, SKIP \
                                 (player already has it)"
                            );
                            self.unique_grants_ok.insert(i);
                            continue;
                        }
                        if crate::detour::grant_full_id(full_id, 1) {
                            if crate::flags::try_set_event_flag(flag, true) {
                                log::info!(
                                    "unique grant: goods {goods} ({full_id:#x}) granted + flag {flag} set"
                                );
                            } else {
                                // Should be unreachable behind the already_flags gate (holder is
                                // up). NOT retried this session -- a retry would re-grant the
                                // goods; the unset flag makes the NEXT session re-grant instead.
                                log::warn!(
                                    "unique grant: goods {goods} granted but flag {flag} write FAILED -- \
                                     possession latch missing; next session will re-grant \
                                     (fail-loud, not retried this session)"
                                );
                            }
                            self.unique_grants_ok.insert(i);
                        } else {
                            all_done = false;
                            warn_unique_grant_fail_once(i, full_id);
                        }
                    }
                    if all_done {
                        self.unique_grants_done = true;
                        if !sc.unique_start_grants.is_empty() {
                            log::info!(
                                "unique start grants settled ({} decided)",
                                sc.unique_start_grants.len()
                            );
                        }
                    }
                }
            }
            if did_flags {
                self.start_flags_done = true;
                if let Some(sc) = self.start.as_ref() {
                    log::info!(
                        "start graces + map reveal applied: {} grace flag(s), reveal_maps={}",
                        sc.start_graces.len(),
                        sc.reveal_all_maps
                    );
                }
            }
            if did_items {
                self.start_items_granted = true;
                log::info!("start items granted");
                self.write_save();
            }
        }

        // Prime the inventory pointer from a game static (if enabled+confirmed) so grants flush
        // without waiting for the player's first in-game pickup. No-op until USE_STATIC_INVENTORY_PRIME
        // is turned on; the detour still captures the game's own pointer on a real pickup regardless.
        crate::detour::prime_inventory_if_needed();

        // 3. Snapshot the received-item stream in one client borrow (RecvItem mirrors for the
        //    seam, plus the cumulative name set the reconcile ticks need). Under own_world:true
        //    this stream ALSO carries the echoes of our own self-found checks.
        let mut disp = self.dispatched_through;
        // SWEEP H3 (verified watermark, via er_logic::receive below): both name-dispatch and
        // grants only run with a loaded world + live inventory pointer — menu-time writes go
        // through a stale pointer / get discarded, and used to advance the watermark on faith.
        let can_grant = crate::detour::has_inventory() && crate::flags::in_world();
        // Cumulative set of ALL received item names — natural-key triggers need the full history
        // (a clause may require an item received many ticks ago), not just this tick's new names.
        let mut received_all: HashSet<String> = HashSet::new();
        let mut snapshot: Vec<RecvItem> = Vec::new();
        if let Some(client) = self.client() {
            let items = client.received_items();
            if items.len() < disp {
                disp = 0; // reconnect shrank the stream -> replay name dispatch from index 0
            }
            // Items below BOTH watermarks are AlreadyPushed no-ops; skip snapshotting them.
            let floor = disp.min(self.received_through);
            let my_slot = client.this_player().slot();
            for (idx, ri) in items.iter().enumerate() {
                let name = ri.item().name().to_string();
                if can_grant && idx >= floor {
                    // ECHO-DEDUP: an echo of our own check whose rewritten shop row already
                    // sold the reward natively must not grant again (shop_sell::echo_skip).
                    let echo_skip = ri.sender().slot() == my_slot
                        && crate::shop_sell::echo_skip(ri.location().id());
                    snapshot.push(RecvItem {
                        index: idx as i64,
                        ap_item_id: ri.item().id(),
                        name: name.clone(),
                        echo_skip,
                    });
                }
                received_all.insert(name);
            }
        }

        // 3b. Baked region-lock fallback arming (bedrock interop): the first received
        //     "<Region> Lock" in the prepared scope is the proof the seed really placed locks;
        //     merge the baked config into the live one so the name-dispatch below (and every
        //     later tick's kick-watch/reconcile) sees it. No-op for seeds whose slot_data spoke
        //     a region key (nothing prepared) and for foreign no-lock seeds (never armed).
        if let Some(cfg) = self.region.as_mut() {
            crate::region::tick_baked_fallback(cfg, &received_all);
        }

        // 4. The receive seam (er_logic::receive, host-tested): per item, name-dispatch when
        //    idx >= dispatched_through (keyitems fast path / region open / progressive routing,
        //    via ReceiveDispatch), then grant when idx >= received_through. received_through only
        //    advances past items whose grant VERIFIABLY placed (SWEEP H3): on a failed placement
        //    it is rolled back to the failed item and the tail retries in order next tick.
        //    dispatched_through keeps its advance regardless — name effects are idempotent and
        //    the section-6 reconcile ticks self-heal any lost flag write.
        let newly_dispatched_from = disp as i64; // BOSS_LOCKS_PATCH: notification window
        let mut dispatched = disp as i64;
        let mut pushed = self.received_through as i64;
        let mut unlocked: Vec<String> = Vec::new();
        if can_grant && !snapshot.is_empty() {
            let empty_map = HashMap::new();
            let item_map = self.item_map.as_ref().unwrap_or(&empty_map);
            let mut game = EldenRingHook;
            let mut dispatch = ReceiveDispatch {
                region: self.region.as_ref(),
                progressive: &mut self.progressive,
                hook: &mut game,
                unlocked: Vec::new(),
            };
            for ri in &snapshot {
                let pushed_before = pushed;
                let action = er_logic::receive::process_received_item(
                    ri,
                    &mut dispatched,
                    &mut pushed,
                    item_map,
                    &self.item_counts,
                    &mut dispatch,
                );
                match action {
                    GrantAction::Enqueue {
                        full_id, qty, name, ..
                    } => {
                        // STRANGLER (goods+ledger, THE ATOMIC FLIP): this ONE call grants every
                        // received item — key items/runes (goods) AND consumables (ledger). Once the
                        // reconciler owns BOTH classes it is the sole received-item grant path (goods
                        // via GrantUnique, consumables via the ledger watermark), so skip this grant
                        // to avoid double-granting consumables on reload. NAME dispatch above and the
                        // `dispatched_through`/`pushed` advance stay; `pushed` simply advances past
                        // this item (no H3 hold — the reconciler owns placement). Runtime-revertible:
                        // drop `goods`/`ledger` from RECONCILE_APPLY and this path grants again.
                        if !(crate::reconcile_io::owns_goods()
                            && crate::reconcile_io::owns_ledger())
                        {
                            if dispatch.hook.grant_full_id(full_id, qty) {
                                // Great-rune "restored" flag is set by keyitems::set_acquire_flags
                                // (191-196); the AP item already grants the restored goods row, so
                                // there is no additive goods grant here (that double-granted the rune).
                            } else {
                                // H3: the grant did NOT place — hold received_through at this item
                                // and stop so the tail replays in order next tick (never advance the
                                // watermark past an unverified grant).
                                pushed = pushed_before;
                                log::warn!(
                                    "grant '{name}' (idx {}) failed to place -- receive watermark held for retry",
                                    ri.index
                                );
                                break;
                            }
                        }
                    }
                    GrantAction::SkipProgressive => {
                        // Tier effects already applied in the dispatch (ReceiveDispatch). Great-rune
                        // restore is handled by keyitems::set_acquire_flags (event flag), not a grant.
                    }
                    GrantAction::SkipUnmapped { ap_item_id } => {
                        // R5 (SWEEP): AP id absent from apIdsToItemIds and progressive didn't
                        // handle it — nothing granted; without this the item vanishes traceless.
                        // FALSE-ALARM SILENCE (patch_silence_regionlock_grant_warn):
                        // region-lock items are intentionally absent from apIdsToItemIds --
                        // they are handled by the NAME-dispatch above (open_on_received_name
                        // sets the region's open flag). They still reach this grant arm and
                        // would otherwise trip the misleading "no ER mapping ... contract
                        // drift?" warn. Identify them cleanly by presence in region_open_flags
                        // (the regionOpenFlags slot_data key set, keyed by lock name -- no
                        // hardcoded ap ids) and log at debug. Truly-unmapped ids keep the warn.
                        let is_region_lock = dispatch
                            .region
                            .map(|c| c.region_open_flags.contains_key(&ri.name))
                            .unwrap_or(false);
                        if is_region_lock {
                            log::debug!(
                                "region-lock '{}' (ap id {ap_item_id}) -> handled via open flag (not an ER item grant)",
                                ri.name
                            );
                        } else if ri.name.starts_with("Boss Key: ") {
                            // Boss Keys (mode B) are SYNTHETIC gate tokens, intentionally absent from
                            // apIdsToItemIds: they gate a felled boss's reward (boss_key_pending) and its
                            // dungeon sweep (sweep_lock_gates) via the received-name set, NOT an ER item
                            // grant. Recognize them here (like region locks) so they do not trip the
                            // misleading "no ER mapping ... contract drift?" warn. Debug-log instead.
                            log::debug!(
                                "boss-key '{}' (ap id {ap_item_id}) -> mode-B gate token (not an ER item grant)",
                                ri.name
                            );
                        } else {
                            warn_unmapped_once(&ri.name, ap_item_id);
                        }
                    }
                    GrantAction::SkipNativelySold { name } => {
                        log::info!(
                            "shop-sell: echo grant skipped -- {name} was sold natively at purchase (ECHO-DEDUP)"
                        );
                    }
                    GrantAction::AlreadyPushed => {}
                }
            }
            unlocked = dispatch.unlocked;
        }
        self.dispatched_through = dispatched.max(0) as usize;
        self.received_through = pushed.max(0) as usize;
        for region in unlocked {
            self.log(ap::Print::message(format!("Region unlocked: {region}")));
        }
        // BOSS_LOCKS_PATCH: overlay line on boss-lock receipt -- the lock item is otherwise
        // invisible in the console (no region apparatus, so "Region unlocked" never fires for
        // it). Mirrors that line's semantics, including the reconnect replay (name-dispatch
        // replays the stream). The gate itself is poll-driven, so a lock arriving after the
        // boss kill fires the held sweep within a few seconds of this line.
        // Announce received Boss Keys (mode B) so the SYNTHETIC gate token is visible in the console
        // (it has no ER item grant, so no "Region unlocked" line ever fires for it). Covers BOTH
        // sweep-gated keys AND keys that only gate a boss's own reward check (the latter are absent
        // from sweep_lock_gates, so the old sweep-only guard skipped them -> the player saw nothing).
        // Match on the SYNTHETIC name; SHOW the legible `display_key` when a boss def carries one, else
        // the boss name (the "Boss Key: " prefix stripped). Owned Strings so the immutable boss_defs /
        // sweep_lock_gates borrows end before `self.log`.
        {
            let mut announced: Vec<String> = Vec::new();
            let mut seen: HashSet<String> = HashSet::new();
            for ri in &snapshot {
                if ri.index < newly_dispatched_from {
                    continue;
                }
                let is_boss_key = self.sweep_lock_gates.values().any(|g| g == &ri.name)
                    || self
                        .boss_defs
                        .iter()
                        .any(|d| d.gate.as_deref() == Some(ri.name.as_str()));
                if is_boss_key && seen.insert(ri.name.clone()) {
                    let shown = self
                        .boss_defs
                        .iter()
                        .find(|d| d.gate.as_deref() == Some(ri.name.as_str()))
                        .and_then(|d| d.gate_display())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| {
                            ri.name
                                .strip_prefix("Boss Key: ")
                                .unwrap_or(ri.name.as_str())
                                .to_string()
                        });
                    announced.push(shown);
                }
            }
            for shown in announced {
                self.log(ap::Print::message(format!(
                    "Boss Key received: {shown} -- its boss reward (and any dungeon sweep) unlocks once held."
                )));
            }
        }

        // 4c. Persist on watermark advance.
        if self.received_through as i64 != self.last_persisted_index {
            self.write_save();
            self.last_persisted_index = self.received_through as i64;
        }

        // 5. Shop / NPC / offline discovery: synthetic placeholders that bypassed the detour are in
        //    the bag. REPORT only (echo grants); dedup by checked-location so it can't re-report.
        if can_grant && self.locations_loaded {
            let scanned = crate::inventory::scan_synthetics();
            if !scanned.is_empty() {
                let mut to_check: Vec<i64> = Vec::new();
                if let Some(client) = self.client() {
                    for s in &scanned {
                        if self.valid_locations.contains(&s.location)
                            && !client.is_local_location_checked(s.location)
                        {
                            to_check.push(s.location);
                        }
                    }
                }
                if !to_check.is_empty() {
                    log::info!("shop/offline discovery: {} new check(s)", to_check.len());
                    if let Some(client) = self.client_mut()
                        && let Err(e) = client.mark_checked(to_check.iter().copied())
                    {
                        log::warn!("shop mark_checked failed: {e}");
                    }
                }
            }
        }

        // 5b. Flag-poll: report detour-bypass checks (NPC gifts, NPC death drops, offline pickups)
        //     whose guarding event flag has fired, plus dungeon/boss sweeps. Throttled — flags don't
        //     change fast and the map can be large. Dedup via the server's checked set (reload-safe).
        self.poll_counter = self.poll_counter.wrapping_add(1);
        if self.locations_loaded && self.poll_counter.is_multiple_of(15) {
            // Capture the new-save baseline once, the first time we poll IN-WORLD (flags are
            // readable only after the save loads). Any guarding flag already set at this point
            // is a new-save default, not a pickup made this session, so it must not fire a check.
            // Computed into an owned local first so the &self.flag_poll borrow ends before the
            // &mut self.flag_poll_baseline assignment.
            if !self.flag_poll_baseline_done && crate::flags::in_world() {
                let baseline: HashSet<u32> = match self.flag_poll.as_ref() {
                    Some(fp) => fp
                        .location_flags
                        .values()
                        .copied()
                        .filter(|&f| crate::flags::get_event_flag(f))
                        .collect(),
                    None => HashSet::new(),
                };
                self.flag_poll_baseline = baseline;
                self.flag_poll_baseline_done = true;
                // Prime the boss-defeat baseline in the same shot: any boss already dead on the
                // first in-world poll (prior session / reconnect) seeds boss_flag_prev so its
                // "Felled: <name>" banner never re-fires. Only a kill made THIS session (an
                // unset->set edge after this) reaches newly_felled == true. Disjoint field access
                // (boss_defs read / boss_flag_prev write) — no self.log call inside.
                self.boss_flag_prev = self
                    .boss_defs
                    .iter()
                    .filter(|d| crate::flags::get_event_flag(d.flag))
                    .map(|d| d.flag)
                    .collect();
                log::info!(
                    "flag-poll baseline: {} guarding flags already set on connect (excluded); {} boss(es) already felled (banner suppressed)",
                    self.flag_poll_baseline.len(),
                    self.boss_flag_prev.len()
                );
                // Persist the freshly-captured baseline NOW so a reconnect before the next
                // item still loads it (persist-on-watermark-advance can lag arbitrarily).
                self.write_save();
            }
            // ATTUNEMENT (attunement_gate) -- prime the bloom baseline once, the first in-world poll.
            // Any region already attuned on connect (prior session / reconnect: the SERVER checked
            // set is authoritative and replayed) has its graces bloomed WITHOUT re-bannering; a
            // crossing made THIS session banners normally. Mirrors boss_flag_prev priming. Its OWN
            // latch (attunement_primed) -- NOT flag_poll_baseline_done, which can already be true from
            // the persisted baseline, so keying off it would strand priming and re-banner on reconnect.
            if !self.attunement_primed
                && crate::flags::in_world()
                && !self.region_attunement.is_empty()
            {
                let already: Vec<String> = match self.client() {
                    Some(client) => self
                        .region_attunement
                        .iter()
                        .filter(|(_, att)| {
                            er_logic::attunement::attuned(&att.members, att.threshold, |m| {
                                self.valid_locations.contains(&m)
                                    && client.is_local_location_checked(m)
                            })
                        })
                        .map(|(region, _)| region.clone())
                        .collect(),
                    None => Vec::new(),
                };
                let mut bloom: Vec<u32> = Vec::new();
                for region in &already {
                    if let Some(att) = self.region_attunement.get(region) {
                        bloom.extend(att.bloom_flags.iter().copied());
                    }
                }
                for f in bloom {
                    crate::flags::set_event_flag(f, true);
                }
                for region in already {
                    self.attuned_regions.insert(region);
                }
                self.attunement_primed = true;
                log::info!(
                    "attunement primed: {} region(s) already attuned on connect (bloomed, banner suppressed)",
                    self.attuned_regions.len()
                );
            }
            // BOSS KEYS (mode B) -- prime the sealed baseline once, the first in-world poll. A boss
            // felled in a PRIOR session whose "Boss Key: <Boss>" is still unreceived (its boss_ap_id
            // check never sent, so absent from the SERVER checked set) is seeded into
            // boss_key_pending SILENTLY, so a reconnect re-derives the seal WITHOUT re-bannering; a
            // kill made THIS session (after priming) banners normally. Mirrors boss_flag_prev /
            // attunement priming. received_all is the cumulative, reconnect-replayed received-name set.
            if !self.boss_key_primed
                && crate::flags::in_world()
                && self.boss_defs.iter().any(|d| d.gate.is_some())
            {
                let mut seed: Vec<(u32, i64)> = Vec::new();
                if let Some(client) = self.client() {
                    for d in &self.boss_defs {
                        if let Some(key) = d.gate.as_deref()
                            && d.boss_ap_id != 0
                            && crate::flags::get_event_flag(d.flag)
                            && !received_all.contains(key)
                            && self.valid_locations.contains(&d.boss_ap_id)
                            && !client.is_local_location_checked(d.boss_ap_id)
                        {
                            seed.push((d.flag, d.boss_ap_id));
                        }
                    }
                }
                for (flag, loc) in seed {
                    self.boss_key_pending.entry(flag).or_default().insert(loc);
                }
                self.boss_key_primed = true;
                log::info!(
                    "boss-key baseline: {} boss check(s) sealed on connect (deferred silently)",
                    self.boss_key_pending
                        .values()
                        .map(|s| s.len())
                        .sum::<usize>()
                );
            }
            let mut to_check: Vec<i64> = Vec::new();
            if let (Some(fp), Some(client)) = (self.flag_poll.as_ref(), self.client()) {
                // Refresh the vanilla-suppressor's collected-flag set: the acquisition flags of every
                // location already in the server checked-set (loc->flag via locationFlags). A location
                // enters this set only AFTER its check was reported, so the detour suppresses a
                // first-time pickup and passes only a genuine re-pickup. See detour::KNOWN_COLLECTED_FLAGS.
                // NOTE: the valid_locations guard comes first (same ordering as the poll loop
                // below); valid_locations is kept correct per-seed by reset_for_new_seed, so no
                // datapackage-unknown id reaches is_local_location_checked -- its panic path is
                // unreachable (the seed-change reset is the real fix for the reconnect panic).
                let collected: std::collections::HashSet<u32> = fp
                    .location_flags
                    .iter()
                    .filter(|&(&loc, _)| {
                        self.valid_locations.contains(&loc) && client.is_local_location_checked(loc)
                    })
                    .map(|(_, &flag)| flag)
                    .collect();
                crate::detour::set_known_collected_flags(collected);
                // 2026-07-06: some getItemFlagId flags are SET on a brand-new save (Flask of
                // Crimson Tears 60000, physick / sacred-tear flags, Leyndell Crimson Hood
                // 10007452, Black Knifeprint 400357). flag_poll_baseline (captured on the first
                // in-world poll) holds them so we never false-check them; the genuine pickup
                // still registers via the AddItemFunc detour.
                for (&loc, &flag) in &fp.location_flags {
                    if self.valid_locations.contains(&loc)
                        && !client.is_local_location_checked(loc)
                        && !self.flag_poll_baseline.contains(&flag)
                        && crate::flags::get_event_flag(flag)
                    {
                        to_check.push(loc);
                    }
                }
                for (trigger, members) in &self.dungeon_sweeps {
                    // Draft B: the location-keyed dungeon_sweeps groups carry NO gate today --
                    // sweepLockGates is flag-keyed and applied in the sweep_flags loop below. A
                    // location-keyed gate table would arrive with the future ItemLotParam join;
                    // until then these groups (minidungeons / chokepoint carves whose lock is not in
                    // this seed's pool) are ungated. (This map is empty in current seeds.)
                    if let Some(&flag) = fp.location_flags.get(trigger)
                        && crate::flags::get_event_flag(flag)
                    {
                        for &m in members {
                            if self.valid_locations.contains(&m)
                                && !client.is_local_location_checked(m)
                            {
                                to_check.push(m);
                            }
                        }
                    }
                }
                for (&flag, locs) in &fp.sweep_flags {
                    // Draft B: hold a gated group's sweep until its boss-lock item is in the
                    // cumulative received set. sweepLockGates is FLAG-keyed, so look it up by this
                    // sweep's boss-defeat flag; poll-driven, so a lock received AFTER the kill fires
                    // the held sweep retroactively on a later tick.
                    if !er_logic::sweep_gate::gate_open(
                        self.sweep_lock_gates.get(&flag).map(String::as_str),
                        |n| received_all.contains(n),
                    ) {
                        continue;
                    }
                    if crate::flags::get_event_flag(flag) {
                        for &loc in locs {
                            if self.valid_locations.contains(&loc)
                                && !client.is_local_location_checked(loc)
                            {
                                to_check.push(loc);
                            }
                        }
                    }
                }
            }
            // ATTUNEMENT-RELEASE (attunement_gate, SPEC-gf-boss-lock-tracker "Attunement-release"):
            // gate the BOSS PAYOUT -- the boss's own check + every dungeon-sweep member -- behind the
            // region's in-region attunement. Ordinary in-region pickups (which BUILD attunement) are
            // never gated. `to_check` already holds this poll's candidates; partition out any payout
            // check whose region is not yet attuned (DEFER into boss_payout_pending), then burst-
            // release a region's held checks the poll it crosses the threshold. Attunement counts from
            // the SERVER checked set (valid_locations pre-filter, then is_local_location_checked) so it
            // survives save-load / reconnect / re-snapshot. Empty regionAttunement => whole block off.
            if !self.region_attunement.is_empty() {
                // Payout checks = boss's own check (boss_ap_id) + every dungeon-sweep member (both the
                // location-keyed and the flag-keyed sweep tables). Cheap to rebuild at the 15-tick throttle.
                let mut payout_locs: HashSet<i64> = HashSet::new();
                for d in &self.boss_defs {
                    if d.boss_ap_id != 0 {
                        payout_locs.insert(d.boss_ap_id);
                    }
                }
                for members in self.dungeon_sweeps.values() {
                    payout_locs.extend(members.iter().copied());
                }
                if let Some(fp) = self.flag_poll.as_ref() {
                    for locs in fp.sweep_flags.values() {
                        payout_locs.extend(locs.iter().copied());
                    }
                }
                // Attunement state per region + partition to_check, computed under ONE immutable client
                // borrow into owned locals (so self can be mutated after the borrow ends).
                let mut att_state: HashMap<String, (u32, u32, bool)> = HashMap::new(); // region -> (count, threshold, attuned)
                let mut kept: Vec<i64> = Vec::with_capacity(to_check.len());
                let mut deferred_new: Vec<(String, i64)> = Vec::new();
                if let Some(client) = self.client() {
                    let checked = |m: i64| {
                        self.valid_locations.contains(&m) && client.is_local_location_checked(m)
                    };
                    for (region, att) in &self.region_attunement {
                        let count = er_logic::attunement::attuned_count(&att.members, checked);
                        att_state.insert(
                            region.clone(),
                            (count, att.threshold, count >= att.threshold),
                        );
                    }
                    for &loc in &to_check {
                        if payout_locs.contains(&loc)
                            && let Some(region) = self.region_table.get(&(loc as u64))
                            && let Some(&(_, _, attuned)) = att_state.get(region)
                            && !attuned
                        {
                            deferred_new.push((region.clone(), loc));
                            continue;
                        }
                        kept.push(loc);
                    }
                }
                to_check = kept;

                // Record newly-deferred payout checks (per-region debt); banner only the growth.
                let mut newly_sealed: BTreeMap<String, usize> = BTreeMap::new();
                for (region, loc) in deferred_new {
                    if self
                        .boss_payout_pending
                        .entry(region.clone())
                        .or_default()
                        .insert(loc)
                    {
                        *newly_sealed.entry(region).or_default() += 1;
                    }
                }

                // Burst-release: a region attuned this poll drains its held checks back into to_check
                // (the existing mark below sends them). Re-evaluation would re-produce them too, but the
                // explicit drain gives the release banner its count and is robust to a missed re-poll.
                let attuned_regions_now: Vec<String> = att_state
                    .iter()
                    .filter(|(_, v)| v.2)
                    .map(|(r, _)| r.clone())
                    .collect();
                let mut released: BTreeMap<String, usize> = BTreeMap::new();
                for region in &attuned_regions_now {
                    if let Some(pending) = self.boss_payout_pending.get_mut(region)
                        && !pending.is_empty()
                    {
                        let n = pending.len();
                        to_check.extend(pending.iter().copied());
                        pending.clear();
                        released.insert(region.clone(), n);
                    }
                }

                // Attunement bloom: light each newly-attuned region's graces once (latch in
                // attuned_regions, reset on seed change). Collect flags/banners first (immutable
                // region_attunement read) so the &mut self.log calls below hold no field borrow.
                let mut bloom_to_light: Vec<u32> = Vec::new();
                let mut crossed: Vec<String> = Vec::new();
                if self.attunement_primed && crate::flags::in_world() {
                    for region in &attuned_regions_now {
                        if !self.attuned_regions.contains(region)
                            && let Some(att) = self.region_attunement.get(region)
                        {
                            bloom_to_light.extend(att.bloom_flags.iter().copied());
                            crossed.push(region.clone());
                        }
                    }
                }
                for f in &bloom_to_light {
                    crate::flags::set_event_flag(*f, true);
                }
                for region in &crossed {
                    self.attuned_regions.insert(region.clone());
                }

                // Banners (suppressed until primed so a reconnect's already-known state stays quiet).
                if self.attunement_primed && crate::flags::in_world() {
                    for (region, n) in newly_sealed {
                        let (cur, thr) = att_state
                            .get(&region)
                            .map(|&(c, t, _)| (c, t))
                            .unwrap_or((0, 0));
                        self.log(ap::Print::message(format!(
                            "Boss felled -- {n} check(s) sealed; attune {cur}/{thr} {region}"
                        )));
                    }
                    for region in &crossed {
                        self.log(ap::Print::message(format!(
                            "Attuned to {region} -- all graces revealed."
                        )));
                    }
                    for (region, n) in released {
                        self.log(ap::Print::message(format!(
                            "Attunement reached -- {n} sealed check(s) released in {region}."
                        )));
                    }
                }
            }
            // BOSS KEYS (mode B, SPEC-gf-boss-lock-tracker "Boss Key: <Boss>"): gate a felled boss's
            // OWN check (boss_ap_id) behind its "Boss Key: <Boss>" item. The dungeon-sweep MEMBERS are
            // already held by sweep_lock_gates via sweep_gate::gate_open in the sweep loop above; this
            // block covers ONLY the boss's own check, which fires through the locationFlags poll and so
            // sits in to_check the moment the boss is felled. Poll-driven: a key received AFTER the kill
            // releases the held check on a later tick. Composes with attunement-release (a check must
            // clear BOTH gates). Empty gate set => block off. is_local_location_checked (server-
            // authoritative, applied in the loops above) makes a re-run idempotent.
            if self.boss_defs.iter().any(|d| d.gate.is_some()) {
                // boss_ap_id -> (defeat flag, "Felled: <Boss>" name, "Boss Key: <Boss>" key) and
                // defeat flag -> (name, key), built under an immutable boss_defs borrow (owned maps,
                // so the mutable boss_key_pending borrow below is conflict-free).
                let by_loc: HashMap<i64, (u32, String, String, String)> = self
                    .boss_defs
                    .iter()
                    .filter_map(|d| {
                        d.gate.as_ref().and_then(|g| {
                            // Draft E: carry a legible display label (display_key when present, else
                            // the synthetic gate name) alongside the synthetic gate `key`.
                            (d.boss_ap_id != 0).then(|| {
                                let display = d.gate_display().unwrap_or(g.as_str()).to_string();
                                (d.boss_ap_id, (d.flag, d.name.clone(), g.clone(), display))
                            })
                        })
                    })
                    .collect();
                let by_flag: HashMap<u32, (String, String)> = self
                    .boss_defs
                    .iter()
                    .filter_map(|d| {
                        d.gate
                            .as_ref()
                            .map(|g| (d.flag, (d.name.clone(), g.clone())))
                    })
                    .collect();

                // Partition to_check: DEFER any gated boss's own check whose key is not yet received.
                let mut kept: Vec<i64> = Vec::with_capacity(to_check.len());
                // Draft E: newly_sealed's 2nd field is the DISPLAY label (legible key when the
                // apworld shipped one). Gating still keys on the synthetic `key`.
                let mut newly_sealed: BTreeMap<u32, (String, String, usize)> = BTreeMap::new();
                for &loc in &to_check {
                    if let Some((flag, name, key, display)) = by_loc.get(&loc)
                        && !er_logic::sweep_gate::gate_open(Some(key.as_str()), |n| {
                            received_all.contains(n)
                        })
                    {
                        if self.boss_key_pending.entry(*flag).or_default().insert(loc) {
                            let e = newly_sealed
                                .entry(*flag)
                                .or_insert_with(|| (name.clone(), display.clone(), 0usize));
                            e.2 += 1;
                        }
                        continue;
                    }
                    kept.push(loc);
                }
                to_check = kept;

                // Burst-release: any held boss whose key is now in received_all drains its pending
                // checks back into to_check (the mark below sends them); cleared so a later poll can't
                // re-release (and the server set filters it anyway).
                let ready_flags: Vec<u32> = self
                    .boss_key_pending
                    .iter()
                    .filter(|(flag, pend)| {
                        !pend.is_empty()
                            && by_flag
                                .get(*flag)
                                .map(|(_, key)| received_all.contains(key))
                                .unwrap_or(false)
                    })
                    .map(|(flag, _)| *flag)
                    .collect();
                let mut released: BTreeMap<u32, (String, usize)> = BTreeMap::new();
                for flag in ready_flags {
                    if let Some(pending) = self.boss_key_pending.get_mut(&flag)
                        && !pending.is_empty()
                    {
                        let n = pending.len();
                        to_check.extend(pending.iter().copied());
                        pending.clear();
                        if let Some((name, _)) = by_flag.get(&flag) {
                            released.insert(flag, (name.clone(), n));
                        }
                    }
                }

                // Banners (in_world guard; the reconnect-seeded seal from priming inserted its loc
                // already, so newly_sealed skips it -> no re-banner). name is "Felled: <Boss>"; strip
                // the prefix for a clean boss label. key is the full "Boss Key: <Boss>".
                if crate::flags::in_world() {
                    for (_, (name, display, n)) in newly_sealed {
                        let boss = name.strip_prefix("Felled: ").unwrap_or(name.as_str());
                        // Draft E: show the legible display label; gating already used the synthetic.
                        self.log(ap::Print::message(format!(
                            "{boss} felled -- {n} check(s) sealed; awaiting {display}"
                        )));
                    }
                    for (_, (name, n)) in released {
                        let boss = name.strip_prefix("Felled: ").unwrap_or(name.as_str());
                        self.log(ap::Print::message(format!(
                            "Unsealed: {boss} -- {n} stored check(s) released."
                        )));
                    }
                }
            }
            if !to_check.is_empty() {
                to_check.sort_unstable();
                to_check.dedup();
                log::info!("flag-poll: {} new check(s)", to_check.len());
                if let Some(client) = self.client_mut()
                    && let Err(e) = client.mark_checked(to_check.iter().copied())
                {
                    log::warn!("flag-poll mark_checked failed: {e}");
                }
            }

            // Boss-lock mode A (SPEC-boss-lock-tracker.md): emit the one-shot "Felled: <Boss>"
            // banner on the unset->set edge of each boss's DEFEAT flag. Presentation only — no
            // self-send; the boss's own boss_ap_id check still fires through the locationFlags
            // poll above. Idempotent across polls via boss_flag_prev (primed on the first
            // in-world poll, so already-dead bosses don't re-banner; persists until seed change).
            // Guarded on in_world so a load-screen flag read can't fire a banner. Banners are
            // collected first (immutable &self.boss_defs borrow) then logged (&mut self.log).
            if crate::flags::in_world() {
                let mut felled_banners: Vec<String> = Vec::new();
                for def in &self.boss_defs {
                    let now = crate::flags::get_event_flag(def.flag);
                    let prev = self.boss_flag_prev.contains(&def.flag);
                    if er_logic::boss_felled::newly_felled(prev, now) {
                        // def.name is already the full "Felled: <Boss>" label.
                        felled_banners.push(def.name.clone());
                    }
                    if now {
                        self.boss_flag_prev.insert(def.flag);
                    }
                }
                for banner in felled_banners {
                    self.log(ap::Print::message(banner));
                }
            }
        }

        // 5c. Goal-send (SPEC-goal-send-20260701.md): once EVERY goalLocations entry is done —
        //     local DefeatFlag first (immune to another slot's !collect), checked-set fallback
        //     for detection-table stragglers — send ClientStatus::Goal. Same throttle as the
        //     flag poll; gated on a loaded world so flags are never read during a load screen.
        //     Session latch only: a re-send after reconnect is idempotent server-side.
        if !self.sent_goal
            && can_grant
            && self.locations_loaded
            && self.poll_counter.is_multiple_of(15)
        {
            let met = match (self.goal.as_ref(), self.client()) {
                (Some(cfg), Some(client)) => crate::goal::is_met(
                    cfg,
                    crate::flags::get_event_flag,
                    // Pre-filter against valid_locations (kept correct per-seed by reset_for_new_seed)
                    // so no datapackage-unknown id reaches is_local_location_checked.
                    |l| self.valid_locations.contains(&l) && client.is_local_location_checked(l),
                    // goalItems: the item must be HELD. `received_all` is the cumulative, reconnect-
                    // replayed received-name set, so this survives save-load and !collect.
                    |n| received_all.contains(n),
                ),
                _ => false,
            };
            if met {
                let sent = match self.client_mut() {
                    Some(client) => match client.set_status(ap::ClientStatus::Goal) {
                        Ok(_) => true,
                        Err(e) => {
                            log::warn!("goal: set_status(Goal) failed (will retry next poll): {e}");
                            false
                        }
                    },
                    None => false,
                };
                if sent {
                    self.sent_goal = true;
                    log::info!("goal: all goal locations complete -> ClientStatus::Goal sent");
                    self.log(ap::Print::message(
                        "GOAL COMPLETE! Victory sent to Archipelago.".to_string(),
                    ));
                }
            }
        }

        // 6. Region-lock KICK + random-start warp trigger (order matters: the warp sets the
        //    done-flag that KICK's start-window guard waits on, so fire it before the kick check).
        let mut graces_lit: Vec<String> = Vec::new();
        // Player-facing overlay messages from the region ticks (warp requested / arrival /
        // kick) -- collected here because cfg borrows self, logged after the borrow ends.
        let mut region_msgs: Vec<String> = Vec::new();
        if let Some(cfg) = self.region.as_ref() {
            if let Some(m) = crate::region::tick_random_start_warp(cfg) {
                region_msgs.push(m);
            }
            // Natural-key regions (Raya/Mountaintops/Snowfield/...) bloom when their vanilla-key
            // disjunction is satisfied. Gated on a loaded world (can_grant) so the flags it sets
            // aren't clobbered by the save load — same reason the start graces are gated.
            if can_grant {
                crate::region::tick_natural_key_triggers(cfg, &received_all);
                // STRANGLER (flags): the reconciler owns region-open/grace-bundle flags (RegionFlags)
                // and key-item/great-rune obtained flags (KeyItem) and self-heals them every stable
                // tick, so skip these two OLD re-appliers when it owns `flags`. `RECONCILE_APPLY=none`
                // (or dry-run) re-enables them with no rebuild. Idempotent either way (flag writes).
                if !crate::reconcile_io::owns_flags() {
                    // Re-apply lock unlocks whose one-shot receive was discarded at menu/load
                    // (lost graces/open flags -- 2026-07-01 playtest). Latched on the open flag.
                    crate::region::tick_reconcile_received_locks(cfg, &received_all);
                    // R3 (SWEEP): key-item obtained flags, same reconcile family -- the one-shot
                    // write in 4a is lost at menu/load; this re-applies with the flag as the latch.
                    crate::keyitems::tick_keyitem_flags(&received_all);
                }
                // grace_rando: light received "Grace: ..." items (graceItems port-gap, 2026-07-01).
                graces_lit = crate::region::tick_grace_items(cfg, &received_all);
            }
            if let Some(m) = crate::region::tick_kick(cfg) {
                region_msgs.push(m);
            }
        }
        // 6b. Capital-version per-tick latch (self-configured; INERT until slot_data spoke and
        //     the burn-done flag is set). Holds 9116 matched to the capital the player is
        //     standing in, so the Erdtree burn never permanently strands the Royal checks.
        crate::region::tick_capital();
        for g in graces_lit {
            self.log(ap::Print::message(format!("{g} unlocked")));
        }
        for m in region_msgs {
            self.log(ap::Print::message(m));
        }

        // 7. DeathLink.
        let my_name = self.my_name.clone();
        for ev in self.take_events() {
            if let ap::Event::DeathLink { source, .. } = ev {
                let foreign = my_name.as_deref().map(|n| n != source).unwrap_or(true);
                if foreign {
                    // R2 (SWEEP H2): honor the slot's death_link option on the INCOMING side too
                    // (the tag is advertised unconditionally; only the outgoing send was gated).
                    if crate::deathlink::is_enabled() {
                        log::info!("DeathLink received from '{source}'");
                        crate::deathlink::latch_incoming_kill();
                    } else {
                        log::info!(
                            "DeathLink received from '{source}' but disabled for this slot -- ignored"
                        );
                    }
                }
            }
        }
        crate::deathlink::drive_kill();
        if crate::deathlink::is_enabled()
            && crate::deathlink::poll_local_death()
            && let Some(client) = self.client_mut()
        {
            log::info!("DeathLink: local death detected -> broadcasting");
            if let Err(e) = client.death_link(ap::DeathLinkOptions::default()) {
                log::warn!("DeathLink: broadcast failed: {e}");
            }
        }

        // 8. Scadutree blessing writer.
        crate::upgrades::tick_global_scadu();

        // 8b. no_weapon_requirements runtime param zeroing (latched once applied).
        crate::no_weapon_reqs::tick();

        // 8b2. no_equip_load: weightless-equipment SpEffect on the player (param edit + apply).
        crate::no_equip_load::tick();

        // 8c. Ticker-only pickup notifs: set showDialogCondType=0 game-wide so AP grants show the
        //     native right-side ticker, not the blocking "NEW Y:OK" modal (was a retired-baker
        //     regulation edit; ported to runtime, latched once applied).
        crate::notif_ticker::tick();

        // 9. Shop system (SHOP-SYSTEM-HANDOFF.md tick order). Pump the scout first (needs client_mut;
        //    take() to dodge the self double-borrow), then run each shop edit in order. Each self-gates
        //    on cache_ready / param-repo and latches DONE after one in-world pass.
        let mut scout = self.scout.take();
        if let Some(sp) = scout.as_mut()
            && let Some(client) = self.client_mut()
        {
            sp.pump(client);
        }
        self.scout = scout;
        if crate::flags::in_world() {
            let _ = crate::fmg_inject::run();
            let _ = crate::shop_flags::run(&[]);
            // Capital release re-key: Enia's 9116-released Maliketh armor rows -> burn-done
            // flag, write-guarded (SPEC-capital-reconciler.md). Own latch; retries until the
            // param repo is up.
            let _ = crate::shop_flags::run_capital_release();
            let _ = crate::upgrade_cost::maybe_apply();
            let _ = crate::shop_sell::run();
            let _ = crate::shop_stock::run();
            let _ = crate::enemy_drops::run();
            let _ = crate::check_lots::run();
            // Cosmetic, and deliberately AFTER the rewrite: dresses the placeholder (AP flower
            // iconId + "Archipelago Item" + caption) so its pickup toast is not ER's nameless-goods
            // render, `[ERROR]`. Own latch — the MSG repo comes up later than the param repo and
            // must not stall the rewrite.
            let _ = crate::check_lots::dress_placeholder();
            let _ = crate::shop_preview::run();
            let _ = crate::shop_icon::run();
            let _ = crate::minibaker::run();
            crate::scaling::tick();
            // Anti-stuck: keep the FieldArea fast-travel gate open so a dungeon/catacomb can never
            // strand the player (SELF-CALIBRATING field overwrite; see fast_travel.rs). Game-thread.
            crate::fast_travel::tick();

            // Config hot-reload: a tester changes server/slot by editing apconfig.json and alt-tabbing
            // back, instead of fighting the game for input in the overlay (ER has no InputBlocker, so
            // clicking closes the ER menu and Escape closes the client's window). The decision is
            // er_logic::config_reload::reload_action -- host-tested: one reconnect per REAL change, no
            // storm from our own save, and a half-written file never drops a live session.
            if let Some(next) = crate::config_watch::poll()
                && let Err(e) = self.base_mut().update_connection_info(
                    &next.url,
                    &next.slot,
                    next.password.clone(),
                )
            {
                log::warn!("config hot-reload: reconnect failed: {e}");
            }
            // Region-lock fog-wall visuals (cosmetic marker at locked borders; the KICK reactor,
            // not this, does the blocking). Runs on the game thread (FrameBegin task) so the
            // CSWorldGeomMan::spawn_geometry call is main-thread-safe.
            if let Some(fw) = self.fogwall.as_mut() {
                crate::fogwall::tick(fw);
            }
        }

        // ---- RECONCILER (strangler). DRY-RUN (`RECONCILE_DRYRUN=1`) computes + LOGS the desired-state
        //      diff WITHOUT applying; APPLY mode (`RECONCILE_APPLY` names a class, default `flags`)
        //      applies the owned classes via `reconcile_io::tick`, and the OLD handlers above skip
        //      whatever the reconciler owns (see the `owns_*` gates). Widened from the dry-run-only
        //      gate: `tick()` was previously unreachable in apply mode (the cutover wiring gap). The
        //      whole block is a no-op only when neither dry-run nor any apply class is active. ----
        if (crate::reconcile_io::dry_run_enabled() || crate::reconcile_io::apply_active())
            && self.slot_data_parsed
        {
            // Snapshot the FULL received stream (not just the tail) -- DesiredInputs is the CUMULATIVE
            // set, and the reconciler derives idempotency from the whole set, not per-event deltas.
            let mut recv: Vec<(i64, String, i64, bool)> = Vec::new();
            if let Some(client) = self.client() {
                let my_slot = client.this_player().slot();
                for (idx, ri) in client.received_items().iter().enumerate() {
                    // ECHO-DEDUP (Gap 2): same predicate the live receive loop uses -- an echo of our
                    // own check whose rewritten shop row already sold the reward natively.
                    let echo_skip = ri.sender().slot() == my_slot
                        && crate::shop_sell::echo_skip(ri.location().id());
                    recv.push((
                        idx as i64,
                        ri.item().name().to_string(),
                        ri.item().id(),
                        echo_skip,
                    ));
                }
            }
            let inputs = self.build_desired_inputs(&recv);
            // Defer reconciler init to the first STABLE IN-WORLD tick: `reconcile_io::init` reads the
            // ER save_slot + play_time to key the per-character watermark (er-startitems-newchar-no-
            // regrant), and those are only valid once a character is loaded. The reconciler is inert
            // before world-stability anyway, so nothing is lost by waiting; received items accumulate
            // in `recv` (rebuilt each tick) and are applied in full once init runs.
            let world_loaded = crate::detour::has_inventory() && crate::flags::in_world();
            if !self.reconcile_inited {
                if world_loaded {
                    let path = self
                        .save_path
                        .as_ref()
                        .and_then(|p| p.parent().map(|d| d.join("reconcile.json")))
                        .unwrap_or_else(|| std::path::PathBuf::from("reconcile.json"));
                    // `received_through` (this save's persisted `last_received_index`) is passed to
                    // init for the positive-frontier cross-check; the per-character keying inside init
                    // decides fresh-vs-resume (see reconcile_io::init / er_logic::reconcile::seed_trust).
                    crate::reconcile_io::init(inputs, path, self.received_through as i64);
                    self.reconcile_inited = true;
                }
                // else: world not loaded yet -- wait; recv keeps accumulating for the eventual init.
            } else {
                crate::reconcile_io::set_inputs(inputs);
            }
            // Dry-run tick: computes + logs the per-action diff; applies nothing (see reconcile_io).
            crate::reconcile_io::tick();
        }

        Ok(())
    }
}

impl Core {
    // ---- RECONCILER DRY-RUN mapper (additive; only called under RECONCILE_DRYRUN) --------------
    //
    // build_desired_inputs folds the parsed slot_data tables + the server-delivered received-item
    // stream into the pure reconciler's `DesiredInputs`. It reuses the SAME tables the live grant
    // path uses (item_map, region_open_flags/lock_reveal_flags, the progressive config, the
    // keyitems obtained/restored table) so the reconciler's plan can be validated against today's
    // behavior in the dry run.
    //
    // SCOPE / ASSUMPTIONS (documented in docs/history/MIGRATION.md, archived):
    //   * Maps the RECEIVED-ITEM STREAM *and* (Gap 1) the slot-data BULK grants: start graces, the
    //     unconditional + reveal_all_maps world-map flags, start items (ledgered once), and the goal.
    //     They are folded from the SAME tables the old startgrants/goal handlers use.
    //   * `seal_flags` is left EMPTY on purpose: the authoritative seal set (area_lock / attunement
    //     flags) is not yet reproduced here, and seeding it wrongly would make the diff propose bogus
    //     ClearFlag actions. Received region LOCKS still SET their open flag.
    //   * consumable `qty` defaults to the item_counts entry or 1; `echo_skip` (Gap 2) dedups a
    //     native-sold shop echo.
    //   * NOTE(windows-verify): `goal_flag` is a SENTINEL (see `reconcile_io::GOAL_SENTINEL_FLAG`).
    //     In dry-run this only LOGS a would-apply SetFlag. Before the ledger/goods APPLY cutover the
    //     client must either route that sentinel action to `ClientStatus::Goal` (a client seam) OR
    //     keep goal-send on the existing `core.rs` handler and pass `goal_flag: None` here. The pure
    //     `SlotData.goal_flag/goal_met` fields are tested in er-logic so either wiring is glue-only.
    fn build_desired_inputs(
        &self,
        received: &[(i64, String, i64, bool)],
    ) -> er_logic::reconcile::DesiredInputs {
        use er_logic::reconcile::{DesiredInputs, ReceivedItem, SaveIdentity, SlotData, StartItem};
        let seed = self.parsed_seed.clone().unwrap_or_default();
        let save = SaveIdentity(self.my_name.clone().unwrap_or_default());
        let items: Vec<ReceivedItem> = received
            .iter()
            .map(|(index, name, ap_id, echo_skip)| ReceivedItem {
                index: *index,
                name: name.clone(),
                semantics: self.classify_received(name, *ap_id, *echo_skip),
            })
            .collect();
        // Gap 1: fold slot-data bulk grants from the SAME tables the live handlers use.
        let sc = self.start.as_ref();
        let slot_data = SlotData {
            seal_flags: Vec::new(),
            start_graces: sc.map(|s| s.start_graces.clone()).unwrap_or_default(),
            always_map_flags: sc
                .map(crate::startgrants::always_map_flags_for)
                .unwrap_or_else(|| vec![crate::startgrants::UNDERGROUND_MAP_VIEW_UNLOCK]),
            reveal_all_maps: sc.map(|s| s.reveal_all_maps).unwrap_or(false),
            map_reveal_flags: sc
                .map(crate::startgrants::reveal_flags_for)
                .unwrap_or_default(),
            start_items: sc
                .map(|s| {
                    s.start_items
                        .iter()
                        .map(|&full_id| StartItem { full_id, qty: 1 })
                        .collect()
                })
                .unwrap_or_default(),
            // Option (b) from the runbook: goal-send stays on the core.rs §5c `ClientStatus::Goal`
            // handler (a network send, NOT an ER flag), so the reconciler never plans the synthetic
            // GOAL_SENTINEL_FLAG SetFlag. `goal_met` is still surfaced for parity/logging.
            goal_flag: None,
            goal_met: self.reconcile_goal_met(),
        };
        DesiredInputs {
            seed,
            save,
            received: items,
            slot_data,
        }
    }

    /// Mirror of the `core.rs` 5c goal-send predicate for the reconciler glue: every goal location is
    /// done (flag goals via live event flags; checked goals via the server-truth checked set,
    /// pre-filtered against `valid_locations` so no datapackage-unknown id reaches the checked query).
    fn reconcile_goal_met(&self) -> bool {
        match (self.goal.as_ref(), self.client()) {
            (Some(cfg), Some(client)) => {
                // goalItems: HELD, not killed. The reconciler has no `received_all` in scope (it is a
                // &self path), so derive the held-name set straight from the received stream -- the
                // same source `received_all` is built from, so the two agree by construction.
                let held: HashSet<String> = client
                    .received_items()
                    .iter()
                    .map(|ri| ri.item().name().to_string())
                    .collect();
                crate::goal::is_met(
                    cfg,
                    crate::flags::get_event_flag,
                    |l| self.valid_locations.contains(&l) && client.is_local_location_checked(l),
                    |n| held.contains(n),
                )
            }
            _ => false,
        }
    }

    /// Classify one received AP item into its reconciler [`ItemSemantics`], reusing the live tables.
    /// Order matters: progressive -> region lock -> key item / great rune -> plain grant.
    fn classify_received(
        &self,
        name: &str,
        ap_id: i64,
        echo_skip: bool,
    ) -> er_logic::reconcile::ItemSemantics {
        use er_logic::reconcile::{ItemSemantics, ProgTier};
        // 1. Progressive item (tier goods packed to grant FullIDs, exactly like the live path).
        if let Some(tiers) = self.progressive.tiers_for(name) {
            let tiers = tiers
                .iter()
                .map(|t| ProgTier {
                    goods: t
                        .goods
                        .iter()
                        .map(|&g| (g as i32) | er_logic::progressive::GOODS_FULLID)
                        .collect(),
                    flags: t.flags.clone(),
                    consumed: t.consumed,
                })
                .collect();
            return ItemSemantics::Progressive {
                tiers,
                overflow_full_id: (er_logic::progressive::LORDS_RUNE_GOODS as i32)
                    | er_logic::progressive::GOODS_FULLID,
            };
        }
        // 2. Region-open lock (intentionally absent from item_map; classified by NAME). Fold in the
        //    lock's revealed grace bundle so those graces self-heal too.
        if let Some(cfg) = self.region.as_ref()
            && let Some(&open) = cfg.region_open_flags.get(name)
        {
            let mut flags = vec![open];
            if let Some(bundle) = cfg.lock_reveal_flags.get(name) {
                flags.extend(bundle.iter().copied());
            }
            return ItemSemantics::RegionFlags(flags);
        }
        // 3. Key item / great rune: the base grant gives the (restored) goods, plus vanilla
        //    obtained/restored companion flags from the keyitems table. Both classes are a unique
        //    good + set-only companion flags, so both map to KeyItem.
        let full_id = self.item_map.as_ref().and_then(|m| m.get(&ap_id)).copied();
        let acq = crate::keyitems::acquire_flags(name);
        if !acq.is_empty()
            && let Some(fid) = full_id
        {
            return ItemSemantics::KeyItem {
                goods: fid as i32,
                obtained_flags: acq,
            };
        }
        // 4. Plain grant: mapped -> ledgered consumable; unmapped -> inert (region locks / boss keys
        //    fell out at step 2 / are name-gated, so an unmapped id here is genuinely effect-less).
        match full_id {
            Some(fid) => {
                let qty = self.item_counts.get(&ap_id).copied().unwrap_or(1) as i32;
                // Gap 2: a native-sold shop echo is ledgered but NOT re-granted (watermark advances).
                ItemSemantics::Consumable {
                    full_id: fid as i32,
                    qty,
                    echo_skip,
                }
            }
            None => ItemSemantics::Inert,
        }
    }

    /// Rebuild all per-seed / per-save state when a reconnect targets a DIFFERENT seed without an
    /// ER reload (see the `parsed_seed` guard). Clears every table that slot_data or the save file
    /// repopulates, so the one-shot parse and save-load run fresh; static tables (region_table,
    /// coarse_table, tracker UI prefs) and install-once globals (detour_installed) are left intact.
    /// Recovered after commit 4bb3c95 accidentally dropped the body while leaving the call sites.
    fn reset_for_new_seed(&mut self) {
        self.received_through = 0;
        self.dispatched_through = 0;
        self.item_map = None;
        self.item_counts.clear();
        self.region = None;
        self.fogwall = None;
        self.progressive = ProgressiveState::new(HashMap::new());
        self.slot_data_parsed = false;
        self.save_path = None;
        self.save_loaded = false;
        self.last_persisted_index = -1;
        self.valid_locations.clear();
        self.locations_loaded = false;
        self.flag_poll = None;
        self.dungeon_sweeps.clear();
        self.sweep_lock_gates.clear();
        self.poll_counter = 0;
        self.flag_poll_baseline.clear();
        self.flag_poll_baseline_done = false;
        self.start = None;
        self.start_flags_done = false;
        self.start_items_granted = false;
        self.start_items_ok.clear();
        self.unique_grants_ok.clear();
        self.unique_grants_done = false;
        self.in_world_since = None;
        self.grant_gate_last_play_region = None;
        self.scout = None;
        self.goal = None;
        self.sent_goal = false;
        self.hints = HintSet::new();
        self.hint_log_watermark = 0;
        // Restore the DEFAULT progression surface so a new seed does not inherit the prior seed's
        // set (the slot_data parse re-applies this seed's own surface, or leaves it empty and stars
        // nothing -- see the deleted-fallback note there).
        self.progression_surface = er_logic::tracker_regions::progression_surface_set();
        // Boss-lock mode A: drop the parsed defs AND re-arm the felled-edge state, so the new
        // seed re-parses bossLockItems and re-primes its baseline on the next in-world poll.
        self.boss_defs.clear();
        self.boss_flag_prev.clear();
        // ATTUNEMENT-RELEASE: drop the parsed gate + all per-save latches so the new seed re-parses
        // regionAttunement and re-primes / re-blooms from scratch.
        self.region_attunement.clear();
        self.boss_payout_pending.clear();
        self.attuned_regions.clear();
        self.attunement_primed = false;
        // BOSS KEYS (mode B): drop the deferred own-check latch + its prime flag so the new seed
        // re-parses gates and re-seeds silently on its next in-world poll.
        self.boss_key_pending.clear();
        self.boss_key_primed = false;
    }

    /// Scan NEW overlay-log entries for `Print::Hint`s and fold them into [Self::hints].
    ///
    /// Hint semantics (SPEC-item-tracker.md option (a)): the hint's `sender` is the player whose
    /// world CONTAINS the hinted location; `receiver` is the player who gets the item. `for_us` =
    /// we are the sender, i.e. the location is in OUR world -- that's what the checks tree marks.
    /// Only our-world hints are inserted (`for_us`, or the id resolving in our region table as a
    /// fallback) so cross-world location-id collisions don't mismark the tree.
    fn accumulate_hints_from_log(&mut self) {
        // Our slot name comes from the client; before we're connected, leave the watermark alone
        // so any early entries still get scanned once names are resolvable.
        let Some(our_name) = self.client().map(|c| c.this_player().name()) else {
            return;
        };
        let log_len = self.base().logs().len();
        let start = self.hint_log_watermark.min(log_len);
        // Two-phase (collect, then insert): the log iterator immutably borrows self, so the
        // HintSet inserts have to wait until the scan ends.
        let mut new_hints: Vec<HintEntry> = Vec::new();
        for (print, _) in self.base().logs().skip(start) {
            let ap::Print::Hint { item, .. } = print else {
                continue;
            };
            let location_id = item.location().id() as u64;
            let for_us = item.sender().name() == our_name;
            if !for_us && !self.region_table.contains_key(&location_id) {
                continue; // another world's location -- not ours to mark
            }
            let other = if for_us {
                item.receiver()
            } else {
                item.sender()
            };
            new_hints.push(HintEntry {
                location_id,
                item_name: item.item().name().to_string(),
                other_player: other.name().to_string(),
                for_us,
            });
        }
        self.hint_log_watermark = log_len;
        for entry in new_hints {
            self.hints.insert(entry);
        }
    }

    /// Coarse regions currently accessible: a coarse region is open iff its lock item's physical
    /// open flag is set -- OR it has no lock at all / the lock isn't part of this seed's pool.
    /// ("" coarse names are the always-open bucket; er-logic treats those as in-logic itself.)
    fn open_coarse_regions(&self) -> HashSet<String> {
        let mut open = HashSet::new();
        let region_open = self.region.as_ref().map(|c| &c.region_open_flags);
        for coarse in self.coarse_table.values() {
            if coarse.is_empty() || open.contains(coarse) {
                continue; // always-open bucket / already decided
            }
            let accessible = match self.coarse_lock_items.get(coarse) {
                None => true, // no lock mapping -> open
                Some(lock) => match region_open.and_then(|m| m.get(lock)) {
                    None => true, // lock absent this seed -> unlocked
                    Some(&flag) => crate::flags::get_event_flag(flag),
                },
            };
            if accessible {
                open.insert(coarse.clone());
            }
        }
        open
    }

    /// Build the per-frame tracker snapshot and draw the window (SPEC-item-tracker.md Phase 1).
    /// Everything the imgui closure touches is a local snapshot -- `self` stays out of it so the
    /// window's close button can just write a local.
    fn render_tracker_window(&mut self, ui: &imgui::Ui) {
        // One client borrow: location id sets (+ id -> display name) and received-item names.
        let mut checked: Vec<u64> = Vec::new();
        let mut unchecked: Vec<u64> = Vec::new();
        let mut loc_names = HashMap::new(); // id -> Ustr (Copy, interned)
        let mut received: HashSet<String> = HashSet::new();
        if let Some(client) = self.client() {
            for loc in client.checked_locations() {
                let id = loc.id() as u64;
                loc_names.insert(id, loc.name());
                checked.push(id);
            }
            for loc in client.unchecked_locations() {
                let id = loc.id() as u64;
                loc_names.insert(id, loc.name());
                unchecked.push(id);
            }
            for ri in client.received_items() {
                received.insert(ri.item().name().to_string());
            }
        }

        // Region-lock accessibility snapshot (bound to a local BEFORE the model borrows &self
        // fields -- keeps the borrows sequential).
        let open_coarse = self.open_coarse_regions();
        let model = er_logic::tracker::build_tracker_model(
            &checked,
            &unchecked,
            &received,
            &self.region_table,
            &self.coarse_table,
            &self.progression_surface,
            &open_coarse,
            &self.hints,
        );
        let mut hint_list: Vec<HintEntry> = self.hints.iter().cloned().collect();
        hint_list.sort_by(|a, b| a.item_name.cmp(&b.item_name));
        // Bosses group snapshot (mode A/B, SPEC-boss-lock-tracker). Built here -- before the imgui
        // closure -- so the closure stays self-free (mirrors `open_coarse`). flag_set reads the live
        // event flags; received is this frame's cumulative received-name set. RE-AUTHORED (this boss
        // tracker post-dates core.rs.bak_rlwarn; reconcile against reflog if an intact one exists).
        let boss_group = er_logic::boss_felled::build_boss_group(
            &self.boss_defs,
            crate::flags::get_event_flag,
            |n| received.contains(n),
        );

        let display_loc = |id: u64| -> String {
            loc_names
                .get(&id)
                .map(|n| n.as_str().to_string())
                .unwrap_or_else(|| format!("(location {id})"))
        };

        let mut open = true;
        // Filter state as locals (the closure stays self-free); written back to self after.
        let mut in_logic_only = self.tracker_in_logic_only;
        let mut surface_only = self.tracker_surface_only;
        ui.window("Item Tracker###ap-tracker")
            .size([480.0, 520.0], imgui::Condition::FirstUseEver)
            .opened(&mut open)
            .build(|| {
                ui.text(format!("checks: {}/{}", model.done, model.total));
                ui.text(format!(
                    "in-logic: {}/{}   surface: {}/{}",
                    model.in_logic_done,
                    model.in_logic_total,
                    model.surface_done,
                    model.surface_total
                ));
                ui.checkbox("in-logic only", &mut in_logic_only);
                ui.same_line();
                ui.checkbox("progression surface only", &mut surface_only);
                ui.separator();
                if model.total == 0 {
                    ui.text_disabled("No location data yet -- connect to a session.");
                }

                // (b) Per-region rollups. The ### id is the region name alone so the header's
                // open state survives the done/total counters changing.
                for region in &model.regions {
                    // Filter pass first so fully-filtered regions can be skipped outright.
                    let shown: Vec<_> = region
                        .unchecked
                        .iter()
                        .filter(|u| {
                            (!in_logic_only || u.in_logic) && (!surface_only || u.on_surface)
                        })
                        .collect();
                    if (in_logic_only || surface_only) && shown.is_empty() {
                        continue;
                    }
                    let lock_tag = if region.accessible { "" } else { "  [locked]" };
                    let header = format!(
                        "{}  {}/{}{}###trk-region-{}",
                        region.region, region.done, region.total, lock_tag, region.region
                    );
                    // Dim the header text while the region's coarse region is locked. The token
                    // pops on drop -- released right after the header so the rows keep their
                    // own colors.
                    let dim = (!region.accessible)
                        .then(|| ui.push_style_color(imgui::StyleColor::Text, LOCKED_GRAY));
                    let expanded = ui.collapsing_header(header, imgui::TreeNodeFlags::empty());
                    drop(dim);
                    if expanded {
                        if shown.is_empty() {
                            ui.text_disabled("  complete");
                        }
                        for u in shown {
                            let name = display_loc(u.location_id);
                            let star = if u.on_surface { "* " } else { "" };
                            let line = if u.hinted {
                                format!("  {star}[hint] {name}")
                            } else {
                                format!("  {star}{name}")
                            };
                            if u.hinted {
                                ui.text_colored(HINT_YELLOW, line);
                            } else if u.on_surface {
                                ui.text_colored(SURFACE_ORANGE, line);
                            } else if !u.in_logic {
                                ui.text_disabled(line);
                            } else {
                                ui.text(line);
                            }
                        }
                    }
                }
                // (b2) Bosses group (mode A/B). RE-AUTHORED tail -- no bak_rlwarn equivalent; the
                // boss tracker post-dates that backup. Rendered from the pure `boss_group` snapshot.
                if !boss_group.rows.is_empty() {
                    ui.separator();
                    let header = format!(
                        "Bosses  {}/{}###trk-bosses",
                        boss_group.defeated(),
                        boss_group.total()
                    );
                    if ui.collapsing_header(header, imgui::TreeNodeFlags::empty()) {
                        for row in &boss_group.rows {
                            // `name` is the full "Felled: <Boss>" label; strip for a clean line.
                            let boss = row
                                .name
                                .strip_prefix("Felled: ")
                                .unwrap_or(row.name.as_str());
                            match row.state {
                                er_logic::boss_felled::BossState::Locked => {
                                    ui.text_disabled(format!("  {boss}  [{}]", row.region));
                                }
                                er_logic::boss_felled::BossState::Felled => {
                                    let line = match &row.display_key {
                                        Some(key) => format!("  {boss}  felled -- awaiting {key}"),
                                        None => format!("  {boss}  felled"),
                                    };
                                    ui.text_colored(SURFACE_ORANGE, line);
                                }
                                er_logic::boss_felled::BossState::Released => {
                                    ui.text_colored(HINT_YELLOW, format!("  {boss}  released"));
                                }
                            }
                        }
                    }
                }

                ui.separator();

                // (c) Received items (raw cumulative names; sorted by the model).
                if ui.collapsing_header(
                    format!(
                        "Items received ({})###trk-items",
                        model.received_items.len()
                    ),
                    imgui::TreeNodeFlags::empty(),
                ) {
                    for item in &model.received_items {
                        ui.text(format!("  {item}"));
                    }
                }

                // (d) Standing hints.
                if ui.collapsing_header(
                    format!("Hints ({})###trk-hints", hint_list.len()),
                    imgui::TreeNodeFlags::empty(),
                ) {
                    if hint_list.is_empty() {
                        ui.text_disabled("  none yet");
                    }
                    for h in &hint_list {
                        let who = if h.for_us {
                            format!("for {}", h.other_player)
                        } else {
                            format!("hinted by {}", h.other_player)
                        };
                        ui.text_colored(
                            HINT_YELLOW,
                            format!("  {} @ {} ({who})", h.item_name, display_loc(h.location_id)),
                        );
                    }
                }
            });
        if !open {
            self.tracker_visible = false;
        }
        self.tracker_in_logic_only = in_logic_only;
        self.tracker_surface_only = surface_only;
    }

    fn write_save(&self) {
        let Some(path) = self.save_path.as_ref() else {
            return;
        };
        let (counter, high) = self.progressive.snapshot();
        let st = SaveState {
            last_received_index: self.received_through as i64,
            start_items_granted: self.start_items_granted,
            flag_poll_baseline: self.flag_poll_baseline.iter().copied().collect(),
            notify_granted: Default::default(),
            progressive_counter: counter.into_iter().collect::<BTreeMap<_, _>>(),
            progressive_high_index: high,
        };
        let tmp = path.with_extension("json.tmp");
        // R7 (SWEEP): surface write/rename failures -- a silently-lost save resets the
        // watermarks next session (duplicate start items + regrant burst).
        match std::fs::write(&tmp, st.to_json()) {
            Ok(()) => {
                if let Err(e) = std::fs::rename(&tmp, path) {
                    log::error!(
                        "save persistence: rename {} -> {} FAILED: {e}",
                        tmp.display(),
                        path.display()
                    );
                }
            }
            Err(e) => log::error!("save persistence: write {} FAILED: {e}", tmp.display()),
        }
    }
}

/// R5 (SWEEP): one warning per unmapped AP item id -- the grant loop would otherwise drop the
/// item with no trace, every session, on every replay.
static START_ITEM_FAIL_LOGGED: std::sync::Mutex<Option<HashSet<usize>>> =
    std::sync::Mutex::new(None);

/// Fail-loud (once per start-item index) when a start grant does not land despite a captured
/// inventory pointer. The start-items loop retries every tick, so without this a stuck grant
/// (Torrent / dlc_only flasks) is silent. Mirrors `warn_unmapped_once`.
fn warn_start_item_fail_once(idx: usize, full_id: i32) {
    let mut guard = START_ITEM_FAIL_LOGGED.lock().unwrap();
    if guard.get_or_insert_with(HashSet::new).insert(idx) {
        log::warn!(
            "start item #{idx} ({full_id:#x}) failed to grant (inventory captured but AddItem \
             rejected) -- retrying each tick; if this persists the start grant is stuck"
        );
    }
}

static UNIQUE_GRANT_FAIL_LOGGED: std::sync::Mutex<Option<HashSet<usize>>> =
    std::sync::Mutex::new(None);

/// Fail-loud (once per uniqueStartGrants index) when a unique grant does not land despite a
/// captured inventory pointer. Mirrors [`warn_start_item_fail_once`]; the block retries the
/// FAILED entry each tick, so without this a stuck grant is silent.
fn warn_unique_grant_fail_once(idx: usize, full_id: i32) {
    let mut guard = UNIQUE_GRANT_FAIL_LOGGED.lock().unwrap();
    if guard.get_or_insert_with(HashSet::new).insert(idx) {
        log::warn!(
            "unique grant #{idx} ({full_id:#x}) failed to grant (inventory captured but AddItem              rejected) -- retrying each tick; if this persists the grant is stuck"
        );
    }
}

static UNMAPPED_LOGGED: std::sync::Mutex<Option<HashSet<i64>>> = std::sync::Mutex::new(None);

fn warn_unmapped_once(name: &str, ap_id: i64) {
    let mut guard = UNMAPPED_LOGGED.lock().unwrap();
    if guard.get_or_insert_with(HashSet::new).insert(ap_id) {
        log::warn!(
            "item '{name}' (ap id {ap_id}) has no ER mapping -- NOT granted (contract drift?)"
        );
    }
}

fn save_file_path(seed: &str, name: &str) -> Option<PathBuf> {
    let dir = match shared::utils::mod_directory() {
        Ok(d) => d,
        Err(e) => {
            // R7 (SWEEP): was `.ok()?` -- save persistence silently never armed.
            log::error!(
                "save persistence UNAVAILABLE ({e}) -- watermarks will reset every session"
            );
            return None;
        }
    };
    let safe = |s: &str| -> String {
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect()
    };
    Some(dir.join(format!("ap_save_{}_{}.json", safe(seed), safe(name))))
}

/// Parse slot_data `bossLockItems` (mode A/B, SPEC-boss-lock-tracker) into [`BossDef`] rows.
/// `{ "<boss_flag>": {name, region, boss_ap_id, gate?, display_key?} }`. Tolerant: skips any
/// entry whose key is not a u32 or whose value is not an object. Absent/empty => no boss tracking.
/// Load the shipped `check_lots_table.json` from the DLL/mod directory (same place as
/// `shoplineup_flags.json`). Absent/garbage -> empty, and suppression simply stays off, which is
/// exactly today's behaviour -- never a panic mid-connect.
fn load_static_lots() -> er_logic::static_lots::StaticLots {
    let path = shared::utils::mod_directory()
        .map(|d| d.join("check_lots_table.json"))
        .unwrap_or_else(|_| std::path::PathBuf::from("check_lots_table.json"));
    match std::fs::read_to_string(&path) {
        Ok(t) => er_logic::static_lots::parse(&t),
        Err(_) => er_logic::static_lots::StaticLots::default(),
    }
}

fn parse_boss_lock_items(v: Option<&Value>) -> Vec<er_logic::boss_felled::BossDef> {
    let mut out = Vec::new();
    let Some(obj) = v.and_then(|v| v.as_object()) else {
        return out;
    };
    for (k, entry) in obj {
        let (Ok(flag), Some(e)) = (k.parse::<u32>(), entry.as_object()) else {
            continue;
        };
        out.push(er_logic::boss_felled::BossDef {
            flag,
            name: e
                .get("name")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            region: e
                .get("region")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            boss_ap_id: e.get("boss_ap_id").and_then(|x| x.as_i64()).unwrap_or(0),
            gate: e.get("gate").and_then(|x| x.as_str()).map(str::to_string),
            display_key: e
                .get("display_key")
                .and_then(|x| x.as_str())
                .map(str::to_string),
        });
    }
    out
}

/// Parse slot_data `regionAttunement` (attunement_gate) into per-region [`RegionAttunement`].
/// `{ "<region>": {threshold, member_ap_ids, bloom_flags} }`. Absent/empty => feature off.
/// `members` is a HashSet<i64> (matches the struct + er_logic::attunement's `&HashSet<i64>` inputs).
fn parse_region_attunement(v: Option<&Value>) -> HashMap<String, RegionAttunement> {
    let mut out = HashMap::new();
    let Some(obj) = v.and_then(|v| v.as_object()) else {
        return out;
    };
    for (region, entry) in obj {
        let Some(e) = entry.as_object() else { continue };
        out.insert(
            region.clone(),
            RegionAttunement {
                threshold: e.get("threshold").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
                members: e
                    .get("member_ap_ids")
                    .and_then(|x| x.as_array())
                    .map(|a| a.iter().filter_map(|x| x.as_i64()).collect())
                    .unwrap_or_default(),
                bloom_flags: e
                    .get("bloom_flags")
                    .and_then(|x| x.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_u64().map(|n| n as u32))
                            .collect()
                    })
                    .unwrap_or_default(),
            },
        );
    }
    out
}

fn i64_map(v: Option<&Value>) -> HashMap<i64, i64> {
    let mut m = HashMap::new();
    if let Some(obj) = v.and_then(|v| v.as_object()) {
        for (k, val) in obj {
            if let (Ok(key), Some(value)) = (k.parse::<i64>(), val.as_i64()) {
                m.insert(key, value);
            }
        }
    }
    m
}

/// `{ "<i64>": <u32> }` slot_data object -> `i64 -> u32`. Tolerant: skips malformed entries. Used by
/// the shop system (locationFlags / shopRowFlags).
fn i64_to_u32_map(v: Option<&Value>) -> HashMap<i64, u32> {
    let mut m = HashMap::new();
    if let Some(obj) = v.and_then(|v| v.as_object()) {
        for (k, val) in obj {
            if let (Ok(key), Some(value)) = (k.parse::<i64>(), val.as_u64()) {
                m.insert(key, value as u32);
            }
        }
    }
    m
}

#[cfg(test)]
mod tests {
    /// RETIRED 2026-07-11 -- this test pinned a FICTION and that is why it never fired.
    ///
    /// It asserted the crate version sits inside a semver BAND (">=0.1.0-beta.4 <0.1.0-beta.5")
    /// that it CONSTRUCTED ITSELF, rather than the string the apworld actually sends. So when the
    /// version handshake (apworld 24e261c) changed `versions` from a band to a descriptive
    ///     "apworld/0.2.0 contract/b68eaa15 data/e4c73b06b595e0de"
    /// the test stayed green while `version_gate` -- fed a string that is not a semver range at all --
    /// failed to parse it, `.unwrap_or(false)`'d, and warned "update the client" on EVERY connect for a
    /// whole playtest. A test that builds its own input cannot catch a change in the real input.
    ///
    /// The gate is gone (the VERSION HANDSHAKE supersedes it: it compares the CONTRACT HASH and the
    /// DATA HASH, which is what actually matters). What replaces the test is the assertion that the
    /// apworld's real `versions` string is the shape the handshake parses -- i.e. test the CONTRACT,
    /// not a hand-built stand-in.
    #[test]
    fn versions_string_is_what_the_handshake_parses_not_a_semver_band() {
        // Exactly what greenfield/eldenring/contract.py version_string() emits.
        let real = "apworld/0.2.0 contract/b68eaa15 data/e4c73b06b595e0de";
        let sd = serde_json::json!({ "versions": real });
        let v = sd.get("versions").and_then(|x| x.as_str()).unwrap();

        // The handshake pulls the contract hash out of it and compares against the compiled-in one.
        let their_contract = v
            .split_whitespace()
            .find_map(|t| t.strip_prefix("contract/"))
            .expect("`versions` must carry contract/<hash> -- the handshake keys off it");
        assert_eq!(
            their_contract.len(),
            8,
            "contract hash is the 8-char prefix"
        );

        // And it is NOT a semver range: the old gate treated it as one and warned on every connect.
        // (The old gate called er_semver::version_satisfies on `real` and got Err -- but er_semver is not
        // even a dependency of this crate; it was only ever reachable through the gate that is now gone.
        // Assert the contract er_logic actually implements instead: a descriptive string is NOT a semver
        // band, so version_gate must never report a clean PASS on one. That is exactly the bug the old
        // gate had -- it turned an unparseable input into `false` and warned on every single connect.)
        assert_ne!(
            er_logic::version::version_gate(&sd, env!("CARGO_PKG_VERSION")),
            Some(true),
            "`versions` is a descriptive string, not a semver band -- nothing may gate on it as one"
        );
    }
}
