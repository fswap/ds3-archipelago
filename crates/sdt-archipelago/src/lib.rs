use windows::Win32::{Foundation::HINSTANCE, System::SystemServices::DLL_PROCESS_ATTACH};

mod core;
mod game;
mod item;
mod slot_data;

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
        item::hook_items();
    }

    shared::initialize::<game::Sekiro>(hmodule, shared::NoOpInputBlocker);

    true
}
