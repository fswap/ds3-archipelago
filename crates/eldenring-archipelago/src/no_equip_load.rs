//! no_equip_load — make equipment weightless so the player is always at light-roll.
//!
//! The game recomputes max equip load every frame from Endurance, so a plain memory write to the
//! computed field reverts instantly (verified 2026-07-18: writing `PlayerGameData.max_equip_load`
//! snapped back at 0ms). So we intervene on the INPUT instead: a permanent, silent SpEffect whose
//! `allItemWeightChangeRate = 0` zeroes equipped-item weight -> equip-load ratio ~0 -> always
//! light roll. This is the data-side equivalent of the "No Weight" AOB hook (which zeroed the
//! weight-sum accumulator), and it avoids the client's first raw code hook.
//!
//! `SP_EFFECT_ID` (20012080) is a pure no-op vanilla `SpEffectParam` row: every field at its
//! default, silent (`vfxId`/`iconId` = -1), and `effectEndurance` = -1 (permanent). It is
//! referenced NOWHERE else in the regulation (verified by cross-referencing the full Smithbox param
//! dump: it occurs exactly once across all 239 param tables -- only as its own row), so editing it
//! to weightless and applying it to the player cannot affect any item, enemy, or system.

use std::sync::atomic::{AtomicBool, Ordering};

use eldenring::cs::{ChrInsExt, SoloParamRepository, SpEffectParam, WorldChrMan};
use fromsoftware_shared::FromStatic;

/// The repurposed vanilla no-op `SpEffectParam` row (see module doc for why it is safe).
const SP_EFFECT_ID: i32 = 20012080;

static ENABLED: AtomicBool = AtomicBool::new(false);
static PARAM_PATCHED: AtomicBool = AtomicBool::new(false);

/// Set from slot_data `options.no_equip_load` at connect.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
    if on {
        log::info!("no_equip_load: enabled (weightless SpEffect {SP_EFFECT_ID})");
    }
}

/// Per-tick. When enabled + in-world: patch our SpEffect row to weightless once, then keep the
/// player carrying it. When disabled: strip it from the player. Idempotent -- applying only when
/// the player doesn't already have it avoids stacking duplicate entries.
pub fn tick() {
    // MENU/BOOT GATE: the param repo and chr sets aren't settled at boot / the main menu, and
    // rows_mut/main_player panic or read stale there. Same signal every other param writer gates on.
    if !crate::flags::in_world() {
        return;
    }
    let enabled = ENABLED.load(Ordering::Relaxed);

    // One-time param edit: allItemWeightChangeRate -> 0 on our chosen row. Only bother once enabled,
    // and latch for the session (the edit is a lower-only no-op if re-run, but we skip anyway).
    if enabled && !PARAM_PATCHED.load(Ordering::Relaxed) {
        // SAFETY: FD4 singleton; only mutated on the single-threaded FrameBegin tick.
        let Ok(repo) = (unsafe { SoloParamRepository::instance_mut() }) else {
            return;
        };
        match repo.get_mut::<SpEffectParam>(SP_EFFECT_ID as u32) {
            Some(row) => {
                row.set_all_item_weight_change_rate(0.0);
                PARAM_PATCHED.store(true, Ordering::Relaxed);
                log::info!("no_equip_load: SpEffect {SP_EFFECT_ID} allItemWeightChangeRate -> 0");
            }
            None => return, // param file not populated yet -- retry next tick
        }
    }

    // Apply to (or strip from) the player. SAFETY: FD4 singleton; single-threaded tick.
    let Ok(wcm) = (unsafe { WorldChrMan::instance_mut() }) else {
        return;
    };
    let Some(player) = wcm.main_player.as_mut() else {
        return;
    };
    let chr = &mut player.chr_ins;
    let has = chr
        .special_effect
        .entries()
        .any(|e| e.param_id == SP_EFFECT_ID);
    if enabled {
        if !has {
            chr.apply_speffect(SP_EFFECT_ID, false);
        }
    } else if has {
        chr.remove_speffect(SP_EFFECT_ID);
    }
}
