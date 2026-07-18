//! Elden Ring Archipelago client, built on the fswap `shared` framework. `DllMain` hands off to the
//! shared lifecycle, which spawns the worker, builds the `Core`, schedules `Core::update` every
//! FrameBegin on the game thread, and applies the Hudhook overlay.

use std::ffi::c_void;

use windows::Win32::{Foundation::HINSTANCE, System::SystemServices::DLL_PROCESS_ATTACH};

mod check_lots;
mod config_watch;
mod contract_gen;
mod core;
mod deathlink;
mod detour;
mod enemy_drops;
mod fast_travel;
mod flagpoll;
mod flags;
mod fmg_inject;
mod fogwall;
mod game;
mod goal;
mod hook_impl;
mod inventory;
mod key_resolver;
mod keyitems;
mod minibaker;
mod no_equip_load;
mod no_weapon_reqs;
mod notif_ticker;
mod params;
mod reconcile_io;
mod region;
mod scaling;
mod scout_proof;
mod shop_flags;
mod shop_icon;
mod shop_preview;
mod shop_sell;
mod shop_stock;
mod startgrants;
mod upgrade_cost;
mod upgrades;
mod warp;
mod warp_hook;
// progressive logic lives in er_logic::progressive (pure, host-tested); no local module needed.

/// DLL entry point — standard 3-arg Win32 `DllMain`. ModEngine2 loads externals via `LoadLibrary`,
/// so the OS invokes this with DLL_PROCESS_ATTACH. (3-arg works under me2 AND me3.)
///
/// We never do real work on the loader lock: hand off to `shared::initialize`, which spawns a worker.
#[unsafe(no_mangle)]
extern "system" fn DllMain(_hinst: HINSTANCE, call_reason: u32, _reserved: *mut c_void) -> bool {
    if call_reason != DLL_PROCESS_ATTACH {
        return true;
    }

    shared::handle_panics::<game::EldenRing>();
    shared::start_logger();

    // The AddItemFunc detour is installed lazily from `Core::update_live` (needs the module loaded
    // + must run off the loader lock), not here. It suppresses synthetic placeholders so they never
    // enter inventory — no inventory scan/removal needed under shared's own_world:false model.

    shared::initialize::<game::EldenRing>(shared::NoOpInputBlocker);
    true
}
