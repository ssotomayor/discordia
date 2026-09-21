//! What CoreAudio says about the default devices, for chasing a Bluetooth
//! headset that stays in HFP after a call. Debug builds on macOS; a no-op
//! everywhere else.

#[cfg(all(target_os = "macos", debug_assertions))]
mod imp {
    use std::ffi::c_void;
    use std::mem::size_of;
    use std::ptr::null;

    use coreaudio_sys::*;

    fn address(selector: u32) -> AudioObjectPropertyAddress {
        AudioObjectPropertyAddress {
            mSelector: selector,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMaster,
        }
    }

    fn get<T: Default + Copy>(object: AudioObjectID, selector: u32) -> Option<T> {
        let addr = address(selector);
        let mut value = T::default();
        let mut size = size_of::<T>() as u32;
        let status = unsafe {
            AudioObjectGetPropertyData(
                object,
                &addr,
                0,
                null(),
                &mut size,
                &mut value as *mut T as *mut c_void,
            )
        };
        (status == 0).then_some(value)
    }

    fn describe(label: &str, selector: u32) -> String {
        let Some(dev) = get::<AudioObjectID>(kAudioObjectSystemObject, selector) else {
            return format!("{label}: no default device");
        };
        let rate = get::<f64>(dev, kAudioDevicePropertyNominalSampleRate).unwrap_or(0.0);
        let here = get::<u32>(dev, kAudioDevicePropertyDeviceIsRunning).unwrap_or(0);
        let anywhere = get::<u32>(dev, kAudioDevicePropertyDeviceIsRunningSomewhere).unwrap_or(0);
        format!(
            "{label} device {dev}: nominal {rate} Hz, running in this process={here}, in any process={anywhere}"
        )
    }

    pub fn log(when: &str) {
        eprintln!(
            "[audio-diag] {when}: {} | {}",
            describe("output", kAudioHardwarePropertyDefaultOutputDevice),
            describe("input", kAudioHardwarePropertyDefaultInputDevice)
        );
    }
}

#[cfg(all(target_os = "macos", debug_assertions))]
pub use imp::log;

#[cfg(not(all(target_os = "macos", debug_assertions)))]
pub fn log(_when: &str) {}
