//! The headset's microphone as Windows sees it: an audio capture endpoint.
//!
//! While blu-ghub holds the G935's G-keys, the headset also leaves its
//! microphone mute to the PC: the mute button and the boom report to
//! blu-ghub instead of muting (see docs/protocol-g935.md), and blu-ghub mutes
//! or unmutes the microphone in Windows. The headset follows Windows' mute
//! setting, its LED with it. With the buttons handed back the headset mutes
//! by itself, and Windows never hears of it.

use anyhow::{Result, anyhow, bail};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_DevNode_PropertyW, CM_Get_Device_Interface_PropertyW, CM_LOCATE_DEVNODE_NORMAL,
    CM_Locate_DevNodeW, CR_SUCCESS,
};
use windows::Win32::Devices::Properties::{
    DEVPKEY_Device_ContainerId, DEVPKEY_Device_InstanceId, DEVPROP_TYPE_GUID, DEVPROP_TYPE_STRING,
    DEVPROPTYPE,
};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{
    DEVICE_STATE_ACTIVE, IMMDeviceEnumerator, MMDeviceEnumerator, eCapture,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize,
};
use windows::core::{GUID, HSTRING, PCWSTR};

/// The container Windows puts devices in that it counts as part of the
/// computer itself (onboard audio, devices on ports it takes for internal).
const COMPUTER: GUID = GUID::from_u128(0x00000000_0000_0000_ffff_ffffffffffff);

/// COM on the calling thread, for as long as this lives.
pub struct Com(());

impl Com {
    pub fn init() -> Result<Com> {
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok()?;
        Ok(Com(()))
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

/// The container a device interface (a HID collection's path) belongs to.
/// Every function of one physical device shares it, audio endpoints
/// included, so it ties the headset's HID collections to its microphone
/// without going by names, which the user can change.
pub fn container_of_interface(path: &str) -> Result<GUID> {
    let path = HSTRING::from(path);
    let mut ty = DEVPROPTYPE::default();
    let mut id = [0u16; 512];
    let mut size = std::mem::size_of_val(&id) as u32;
    let cr = unsafe {
        CM_Get_Device_Interface_PropertyW(
            &path,
            &DEVPKEY_Device_InstanceId,
            &mut ty,
            Some(id.as_mut_ptr().cast()),
            &mut size,
            0,
        )
    };
    if cr != CR_SUCCESS || ty != DEVPROP_TYPE_STRING {
        bail!("no device instance for {path} ({cr:?})");
    }
    let container = container_of_instance(PCWSTR(id.as_ptr()))?;
    // Then it shares its container with the computer's own devices, and
    // its microphone cannot be told from theirs.
    if container == COMPUTER || container == GUID::zeroed() {
        bail!("Windows counts the headset's dongle as part of the computer (an internal port?)");
    }
    Ok(container)
}

pub fn container_of_instance(id: PCWSTR) -> Result<GUID> {
    let mut node = 0u32;
    let cr = unsafe { CM_Locate_DevNodeW(&mut node, id, CM_LOCATE_DEVNODE_NORMAL) };
    if cr != CR_SUCCESS {
        bail!("device not found ({cr:?})");
    }
    let mut ty = DEVPROPTYPE::default();
    let mut container = GUID::zeroed();
    let mut size = std::mem::size_of::<GUID>() as u32;
    let cr = unsafe {
        CM_Get_DevNode_PropertyW(
            node,
            &DEVPKEY_Device_ContainerId,
            &mut ty,
            Some((&mut container as *mut GUID).cast()),
            &mut size,
            0,
        )
    };
    if cr != CR_SUCCESS || ty != DEVPROP_TYPE_GUID {
        bail!("no container id ({cr:?})");
    }
    Ok(container)
}

/// A microphone's mute switch in Windows.
pub struct Mic {
    volume: IAudioEndpointVolume,
}

impl Mic {
    /// The active recording device in `container`, if Windows has exactly
    /// one; more than one is an error, as which is the headset's would be a
    /// guess. Needs COM on the calling thread ([`Com`]).
    pub fn find(container: GUID) -> Result<Option<Mic>> {
        let devices: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }?;
        let all = unsafe { devices.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE) }?;
        let mut found = Vec::new();
        for n in 0..unsafe { all.GetCount() }? {
            let device = unsafe { all.Item(n) }?;
            let id = unsafe { device.GetId() }?;
            let text = unsafe { id.to_string() };
            unsafe { CoTaskMemFree(Some(id.0 as *const _)) };
            // An endpoint is a device of its own, under this instance id.
            let instance = HSTRING::from(format!("SWD\\MMDEVAPI\\{}", text?));
            if container_of_instance(PCWSTR(instance.as_ptr())).ok() == Some(container) {
                found.push(device);
            }
        }
        match found.as_slice() {
            [] => Ok(None),
            [device] => {
                let volume: IAudioEndpointVolume = unsafe { device.Activate(CLSCTX_ALL, None) }?;
                Ok(Some(Mic { volume }))
            }
            more => bail!("{} microphones share the headset's device container", more.len()),
        }
    }

    pub fn muted(&self) -> Result<bool> {
        Ok(unsafe { self.volume.GetMute() }?.as_bool())
    }

    /// Windows passes a change on to the device, but setting the state it
    /// believes the device already has does nothing at all.
    pub fn set(&self, muted: bool) -> Result<()> {
        unsafe { self.volume.SetMute(muted, std::ptr::null()) }
            .map_err(|e| anyhow!("setting the microphone's mute: {e}"))
    }

    /// Unmute, so that the headset hears it even when Windows believes the
    /// microphone is already on: the headset mutes by itself when its boom
    /// is raised, and Windows never learns of it. Briefly muting first is
    /// harmless.
    pub fn unmute(&self) -> Result<()> {
        if !self.muted()? {
            self.set(true)?;
        }
        self.set(false)
    }

    /// Mute when Windows' state cannot be trusted in either direction (the
    /// headset may have muted or unmuted by itself before blu-ghub had its
    /// buttons). If Windows believes it muted, it is unmuted first so the
    /// mute goes through, which can open the microphone for a moment: only
    /// for a press of the mute button, never for the boom, which nobody
    /// raises to be heard. A plain mute needs no forcing while blu-ghub has
    /// the buttons, as muting is then the one thing the headset does by
    /// itself.
    pub fn mute_surely(&self) -> Result<()> {
        if self.muted()? {
            self.set(false)?;
        }
        self.set(true)
    }

    /// Flip the mute; returns the new state.
    pub fn toggle(&self) -> Result<bool> {
        let muted = !self.muted()?;
        self.set(muted)?;
        Ok(muted)
    }
}
