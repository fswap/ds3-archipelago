use std::time::Duration;

use anyhow::Result;
use fromsoftware_shared::{FromStatic, Program, SharedTaskImpExt};
use sekiro::sprj::*;

pub struct Sekiro;

impl shared::Game for Sekiro {
    type Core = crate::core::Core;
    type GraphicsHooks = hudhook::hooks::dx11::ImguiDx11Hooks;
    type InputBlocker = shared::NoOpInputBlocker;
    const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

    unsafe fn run_recurring_task(mut task: impl FnMut() + 'static + Send) -> Result<()> {
        unsafe { SprjTaskImp::instance() }?
            .run_recurring(move |_: &'_ usize| task(), SprjTaskGroupIndex::FrameBegin);
        Ok(())
    }

    fn wait_for_system_init() -> Result<()> {
        sekiro::util::system::wait_for_system_init(&Program::current(), Duration::MAX)?;
        Ok(())
    }

    unsafe fn is_main_menu() -> bool {
        // If MapItemMan isn't available, that usually means we're on the
        // main menu. There's probably a better way to detect that but we
        // don't know it yet.
        unsafe { MapItemMan::instance() }.is_err()
    }

    unsafe fn force_cursor_visible() {
        if let Ok(man) = unsafe { MenuMan::instance() } {
            man.set_menu_mode(true);
        }
    }

    unsafe fn is_menu_open() -> bool {
        unsafe { Self::is_main_menu() || MenuMan::instance().is_ok_and(|mm| mm.is_menu_mode()) }
    }
}
