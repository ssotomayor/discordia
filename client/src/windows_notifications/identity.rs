use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::Path;

use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoUninitialize, IPersistFile, STGM_READWRITE,
};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
use windows::core::{BSTR, GUID, Interface, PCWSTR};

const APP_ID_KEY: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
    pid: 5,
};

struct Apartment;
impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: a successful CoInitializeEx owns exactly one matching uninitialization.
        unsafe { CoUninitialize() };
    }
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

pub(super) fn register() -> Result<(), String> {
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let programs = dirs::data_dir()
        .ok_or("Windows application data directory is unavailable")?
        .join("Microsoft/Windows/Start Menu/Programs/Discordia.lnk");
    ensure_shortcut(&programs, &executable)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

pub(super) fn ensure_shortcut(
    shortcut: &Path,
    executable: &Path,
) -> Result<bool, Box<dyn std::error::Error>> {
    // SAFETY: registration is synchronous on this thread, and Apartment balances successful initialization.
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()? };
    let _apartment = Apartment;
    let filename = wide(shortcut);
    let target = wide(executable);
    // SAFETY: COM is initialized and all UTF-16 buffers and property values outlive their calls.
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        let file: IPersistFile = link.cast()?;
        let properties: IPropertyStore = link.cast()?;
        if shortcut.exists() {
            file.Load(PCWSTR(filename.as_ptr()), STGM_READWRITE)?;
            let mut current = vec![0u16; 32768];
            link.GetPath(&mut current, std::ptr::null_mut(), 0)?;
            let end = current
                .iter()
                .position(|c| *c == 0)
                .unwrap_or(current.len());
            let current = std::path::PathBuf::from(std::ffi::OsString::from_wide(&current[..end]));
            if std::fs::canonicalize(current)? != std::fs::canonicalize(executable)? {
                return Err("Notification shortcut belongs to another Discordia executable".into());
            }
            let existing = properties.GetValue(&APP_ID_KEY)?;
            if BSTR::try_from(&existing).is_ok_and(|id| id == super::APP_ID) {
                return Ok(false);
            }
        } else {
            if let Some(parent) = shortcut.parent() {
                std::fs::create_dir_all(parent)?;
            }
            link.SetPath(PCWSTR(target.as_ptr()))?;
            link.SetDescription(windows::core::w!("Discordia"))?;
        }
        properties.SetValue(&APP_ID_KEY, &PROPVARIANT::from(super::APP_ID))?;
        properties.Commit()?;
        file.Save(PCWSTR(filename.as_ptr()), true)?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_existing_shortcut_is_repaired_without_changing_its_arguments() {
        let directory =
            std::env::temp_dir().join(format!("discordia-shortcut-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let executable = directory.join("Discordia.exe");
        let shortcut = directory.join("Discordia.lnk");
        std::fs::write(&executable, b"test executable").unwrap();
        // SAFETY: this test balances COM initialization and uses live, NUL-terminated buffers.
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap();
            let _apartment = Apartment;
            let link: IShellLinkW =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
            link.SetPath(PCWSTR(wide(&executable).as_ptr())).unwrap();
            link.SetArguments(windows::core::w!("--preserved-argument"))
                .unwrap();
            link.cast::<IPersistFile>()
                .unwrap()
                .Save(PCWSTR(wide(&shortcut).as_ptr()), true)
                .unwrap();
            assert!(ensure_shortcut(&shortcut, &executable).unwrap());
            assert!(!ensure_shortcut(&shortcut, &executable).unwrap());
            let verify: IShellLinkW =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
            verify
                .cast::<IPersistFile>()
                .unwrap()
                .Load(PCWSTR(wide(&shortcut).as_ptr()), STGM_READWRITE)
                .unwrap();
            let mut arguments = [0u16; 256];
            verify.GetArguments(&mut arguments).unwrap();
            let end = arguments.iter().position(|c| *c == 0).unwrap();
            assert_eq!(
                String::from_utf16_lossy(&arguments[..end]),
                "--preserved-argument"
            );
        }
        std::fs::remove_file(shortcut).unwrap();
        std::fs::remove_file(executable).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn registration_is_idempotent_and_does_not_retarget_another_installation() {
        let directory =
            std::env::temp_dir().join(format!("discordia-shortcut-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let executable = directory.join("Discordia.exe");
        let other = directory.join("Other.exe");
        let shortcut = directory.join("Discordia.lnk");
        std::fs::write(&executable, b"test executable").unwrap();
        std::fs::write(&other, b"another executable").unwrap();
        assert!(ensure_shortcut(&shortcut, &executable).unwrap());
        assert!(!ensure_shortcut(&shortcut, &executable).unwrap());
        assert!(ensure_shortcut(&shortcut, &other).is_err());
        assert!(!ensure_shortcut(&shortcut, &executable).unwrap());
        for file in [shortcut, executable, other] {
            std::fs::remove_file(file).unwrap();
        }
        std::fs::remove_dir(directory).unwrap();
    }
}
