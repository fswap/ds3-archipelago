use darksouls3_extra::input::InputBlocker;
use windows::Win32::{Foundation::HINSTANCE, System::SystemServices::DLL_PROCESS_ATTACH};

mod core;
mod game;
mod item;
mod save_data;
mod slot_data;

use save_data::SaveData;

/// The entrypoint called when the DLL is first loaded.
///
/// This is where we set up the whole mod and start waiting for the app itself
/// to be initialized enough for us to start doing real things.
#[unsafe(no_mangle)]
extern "C" fn DllMain(hmodule: HINSTANCE, call_reason: u32) -> bool {
    if call_reason != DLL_PROCESS_ATTACH {
        return true;
    }

    shared::handle_panics();
    shared::start_logger();

    // Set up hooks in the main thread to mitigate the risk of the game code
    // executing them while they're being modified.

    // Safety: We only hook these functions here specifically.
    unsafe {
        SaveData::hook();
        item::hook_items();
    }

    let blocker =
        unsafe { InputBlocker::get_instance() }.expect("Failed to initialize input blocker");

    shared::initialize::<game::DarkSoulsIII>(hmodule, game::DS3InputBlocker(blocker));

    true
}
