use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::{cmp, ffi::OsString, io, mem, mem::MaybeUninit, sync::LazyLock};

use anyhow::{Context, Error, Result};
use imgui::*;
use mint::Vector2;
use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, HMODULE, MAX_PATH};
use windows::Win32::System::ProcessStatus::{ENUM_PROCESS_MODULES_EX_FLAGS, EnumProcessModulesEx};
use windows::Win32::System::{LibraryLoader::GetModuleFileNameW, Threading::GetCurrentProcess};
use windows_result::Error as WindowsError;

/// Returns the path to the parent directory of the mod.
pub fn mod_directory<'a>() -> Result<&'a Path> {
    // We should use OnceLock.get_or_try_init once it's stable.
    static LOCK: LazyLock<Result<PathBuf>> = LazyLock::new(load_mod_directory);

    match *LOCK {
        Ok(ref path) => Ok(path.as_path()),
        // We can't reuse the existing error, because it's owned by the
        // LazyLock. Instead, try to reproduce it.
        Err(_) => match load_mod_directory() {
            // If we can't reproduce it, just provide a simple error.
            Ok(_) => Err(Error::msg("failed to locate mod directory")),
            Err(err) => Err(err),
        },
    }
}

/// Loads [mod_directory] without caching.
fn load_mod_directory() -> Result<PathBuf> {
    println!("Locating mod directory...");
    match try_load_mod_directory(0x100) {
        Ok(TryLoadModDirectoryResult::Path(path)) => Ok(path),
        Ok(TryLoadModDirectoryResult::TryAgain(size)) => match try_load_mod_directory(size) {
            Ok(TryLoadModDirectoryResult::Path(path)) => Ok(path),
            Ok(TryLoadModDirectoryResult::TryAgain(next_size)) => Err(Error::msg(format!(
                "got multiple resize requests, {:x} and {:x}",
                size, next_size
            ))),
            Err(err) => Err(err),
        },
        Err(err) => Err(err),
    }
    .context("failed to locate mod directory")
}

/// Passes an array of the given [size] to [EnumProcessModules] to attempt to
/// find the mod location.
///
/// Returns `None` if the mod location wasn't found *and* more
fn try_load_mod_directory(size: u32) -> Result<TryLoadModDirectoryResult> {
    let mut modules = vec![MaybeUninit::<HMODULE>::uninit(); size as usize];
    let module_size = mem::size_of::<HMODULE>() as u32;
    let mut bytes_needed: u32 = 0;
    unsafe {
        EnumProcessModulesEx(
            GetCurrentProcess(),
            modules.as_mut_ptr().cast(),
            module_size * size,
            &raw mut bytes_needed,
            // Only list 64-bit modules, since we know me3 is 64-bit.
            ENUM_PROCESS_MODULES_EX_FLAGS(2),
        )?;
    }

    let modules_needed = bytes_needed / module_size;
    println!("  Found {} loaded DLLs", modules_needed);

    let modules = &modules[..cmp::min(modules_needed, size) as usize];
    for module in modules {
        let mut path = get_module_path(unsafe { module.assume_init() })?;
        if path.file_name().and_then(|op| op.to_str()) == Some("me3_mod_host.dll") {
            println!("  Found ME3 DLL: {:?}", path);
            if let Some(parent) = path.parent()
                && parent.ends_with("bin/win64")
            {
                // The Linux ME3 distribution has me3_mod_host.dll in a deeper
                // directory than the Windows distribution, so pop one extra
                // layer off.
                path.pop();
            }
            path.pop();
            path.pop();
            println!("  Mod path: {:?}", path);
            return Ok(TryLoadModDirectoryResult::Path(path));
        }
    }

    if modules_needed > size {
        println!("  There are more DLLs to check, retrying");
        Ok(TryLoadModDirectoryResult::TryAgain(modules_needed))
    } else {
        println!(
            "  All loaded DLLs: {:?}",
            modules
                .iter()
                .map(|m| get_module_path(unsafe { m.assume_init() }))
        );
        Err(Error::msg("me3_mod_host.dll isn't loaded in this process"))
    }
}

/// The value returned by [try_load_mod_directory]
enum TryLoadModDirectoryResult {
    /// The path to the mod directory.
    Path(PathBuf),

    /// The number of [HMODULE]s necessary to load all DLLs in this process.
    TryAgain(u32),
}

/// Returns the full path to [module].
fn get_module_path(module: HMODULE) -> Result<PathBuf> {
    // `GetModuleFileNameW` doesn't have any way to indicate how much room is
    // necessary for the file, so we have to progressively increase our
    // allocation until we hit the appropriate size.
    let mut size = usize::try_from(MAX_PATH)?;
    let mut filename: Vec<u16>;
    const GROWTH_FACTOR: f64 = 1.5;
    loop {
        filename = vec![0; size];
        let n = unsafe { GetModuleFileNameW(module, &mut filename) } as usize;
        if n == 0 {
            return Err(WindowsError::from_thread().into());
        } else if n == filename.capacity()
            && io::Error::last_os_error()
                .raw_os_error()
                .is_some_and(|c| i32::try_from(ERROR_INSUFFICIENT_BUFFER.0).is_ok_and(|e| c == e))
        {
            size = (size as f64 * GROWTH_FACTOR) as usize;
        } else {
            filename.truncate(n);
            break;
        }
    }

    Ok(PathBuf::from(OsString::from_wide(&filename)))
}

pub trait PopupModalExt {
    /// Sets the size of the modal dialog.
    fn size(self, size: impl Into<Vector2<f32>>, condition: Condition) -> Self;
}

impl<Label> PopupModalExt for PopupModal<'_, '_, Label> {
    fn size(self, size: impl Into<Vector2<f32>>, condition: Condition) -> Self {
        unsafe { imgui_sys::igSetNextWindowSize(size.into().into(), condition as i32) };
        self
    }
}
