//! The equaliser Windows actually plays the G935 through: Logitech's audio
//! processing ("Logitech HX2E Surround Sound Effect", an APO that G HUB's
//! audio driver attaches to the headset's playback endpoint). It keeps its
//! settings among the endpoint's effect properties, which Windows' own Sound
//! control panel changes without administrator rights through the audio
//! policy service; that interface (IPolicyConfig) is undocumented, and is
//! described here as other tools use it.
//!
//! The equaliser property, decoded from a real one (see [`LogiEq`]): 10
//! band frequencies, 10 gains in dB and 10 Q values, all little-endian
//! `f32`, then 40 bytes not understood, kept as they are. (The registry
//! copy has 8 bytes more in front: the stored value's type.)

use anyhow::{Result, anyhow, bail};
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::{
    DEVICE_STATE_ACTIVE, IMMDeviceEnumerator, MMDeviceEnumerator, eRender,
};
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0, PropVariantClear,
};
use windows::Win32::System::Com::{
    BLOB, CLSCTX_ALL, CoCreateInstance, CoTaskMemAlloc, CoTaskMemFree,
};
use windows::Win32::System::Variant::VT_BLOB;
use windows::core::{GUID, HSTRING, PCWSTR};

// The method names are the interface's own.
#[allow(non_snake_case)]
mod policy_config {
    use windows::Win32::Foundation::PROPERTYKEY;
    use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
    use windows::core::{BOOL, HRESULT, IUnknown, IUnknown_Vtbl, PCWSTR, interface};

    /// The audio policy service's configuration interface (Windows 7 and later).
    #[interface("f8679f50-850a-41cf-9c72-430f290290c8")]
    pub unsafe trait IPolicyConfig: IUnknown {
        fn GetMixFormat(&self, device: PCWSTR, format: *mut *mut core::ffi::c_void) -> HRESULT;
        fn GetDeviceFormat(
            &self,
            device: PCWSTR,
            default: BOOL,
            format: *mut *mut core::ffi::c_void,
        ) -> HRESULT;
        fn ResetDeviceFormat(&self, device: PCWSTR) -> HRESULT;
        fn SetDeviceFormat(
            &self,
            device: PCWSTR,
            endpoint: *mut core::ffi::c_void,
            mix: *mut core::ffi::c_void,
        ) -> HRESULT;
        fn GetProcessingPeriod(
            &self,
            device: PCWSTR,
            default: BOOL,
            default_period: *mut i64,
            min_period: *mut i64,
        ) -> HRESULT;
        fn SetProcessingPeriod(&self, device: PCWSTR, period: *mut i64) -> HRESULT;
        fn GetShareMode(&self, device: PCWSTR, mode: *mut core::ffi::c_void) -> HRESULT;
        fn SetShareMode(&self, device: PCWSTR, mode: *mut core::ffi::c_void) -> HRESULT;
        fn GetPropertyValue(
            &self,
            device: PCWSTR,
            fx_store: BOOL,
            key: *const PROPERTYKEY,
            value: *mut PROPVARIANT,
        ) -> HRESULT;
        fn SetPropertyValue(
            &self,
            device: PCWSTR,
            fx_store: BOOL,
            key: *const PROPERTYKEY,
            value: *mut PROPVARIANT,
        ) -> HRESULT;
    }

    /// An effect property, read.
    pub unsafe fn get(
        p: &IPolicyConfig,
        device: PCWSTR,
        key: &PROPERTYKEY,
        value: *mut PROPVARIANT,
    ) -> HRESULT {
        unsafe { p.GetPropertyValue(device, true.into(), key, value) }
    }

    /// An effect property, set.
    pub unsafe fn set(
        p: &IPolicyConfig,
        device: PCWSTR,
        key: &PROPERTYKEY,
        value: *mut PROPVARIANT,
    ) -> HRESULT {
        unsafe { p.SetPropertyValue(device, true.into(), key, value) }
    }
}

use policy_config::IPolicyConfig;

const CLSID_POLICY_CONFIG: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

/// The Logitech processing's equaliser property.
pub const LOGI_EQ: PROPERTYKEY =
    PROPERTYKEY { fmtid: GUID::from_u128(0x42d67d07_c2b8_47f7_8783_109668632ec6), pid: 0 };

/// The active playback endpoints in a device container (the G935's), as
/// endpoint ids. COM must be initialised on this thread.
pub fn render_endpoints(container: GUID) -> Result<Vec<String>> {
    let devices: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }?;
    let all = unsafe { devices.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) }?;
    let mut out = Vec::new();
    for n in 0..unsafe { all.GetCount() }? {
        let device = unsafe { all.Item(n) }?;
        let id = unsafe { device.GetId() }?;
        let text = unsafe { id.to_string() };
        unsafe { CoTaskMemFree(Some(id.0 as *const _)) };
        let text = text?;
        let instance = HSTRING::from(format!("SWD\\MMDEVAPI\\{text}"));
        if crate::audio::container_of_instance(PCWSTR(instance.as_ptr())).ok() == Some(container) {
            out.push(text);
        }
    }
    Ok(out)
}

fn policy() -> Result<IPolicyConfig> {
    Ok(unsafe { CoCreateInstance(&CLSID_POLICY_CONFIG, None, CLSCTX_ALL) }?)
}

/// An endpoint's effect property, as bytes (it must be a blob).
pub fn get_fx(endpoint: &str, key: PROPERTYKEY) -> Result<Vec<u8>> {
    let p = policy()?;
    let id = HSTRING::from(endpoint);
    let mut v = PROPVARIANT::default();
    unsafe { policy_config::get(&p, PCWSTR(id.as_ptr()), &key, &mut v) }.ok()?;
    let out = unsafe {
        let inner = &v.Anonymous.Anonymous;
        if inner.vt != VT_BLOB {
            let vt = inner.vt;
            let _ = PropVariantClear(&mut v);
            bail!("the property is not a blob (type {})", vt.0);
        }
        let b = inner.Anonymous.blob;
        let bytes = if b.pBlobData.is_null() {
            Vec::new()
        } else {
            std::slice::from_raw_parts(b.pBlobData, b.cbSize as usize).to_vec()
        };
        let _ = PropVariantClear(&mut v);
        bytes
    };
    Ok(out)
}

/// Set an endpoint's effect property to a blob of `bytes`, through the
/// audio policy service (as the Sound control panel does).
pub fn set_fx(endpoint: &str, key: PROPERTYKEY, bytes: &[u8]) -> Result<()> {
    let p = policy()?;
    let id = HSTRING::from(endpoint);
    // The blob in COM's own memory, and never freed here: the service frees
    // it (seen: given Rust's memory instead, the process died of a corrupt
    // heap right after the write, which had gone through). Should it not,
    // a write leaks its few hundred bytes; there are few.
    let data = unsafe { CoTaskMemAlloc(bytes.len().max(1)) } as *mut u8;
    if data.is_null() {
        bail!("no memory for the effect property");
    }
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len()) };
    let mut v = PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: core::mem::ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_BLOB,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: PROPVARIANT_0_0_0 {
                    blob: BLOB { cbSize: bytes.len() as u32, pBlobData: data },
                },
            }),
        },
    };
    unsafe { policy_config::set(&p, PCWSTR(id.as_ptr()), &key, &mut v) }
        .ok()
        .map_err(|e| anyhow!("setting the effect property: {e}"))
}

/// The first of the container's playback endpoints with Logitech's
/// equaliser on it, and the equaliser.
pub fn find_eq(container: GUID) -> Result<Option<(String, LogiEq)>> {
    for ep in render_endpoints(container)? {
        if let Ok(b) = get_fx(&ep, LOGI_EQ)
            && let Ok(eq) = LogiEq::parse(&b)
        {
            return Ok(Some((ep, eq)));
        }
    }
    Ok(None)
}

/// Set the equaliser's gains on `endpoint`, keeping the rest as it is.
pub fn set_gains(endpoint: &str, gains: &[f32]) -> Result<LogiEq> {
    let mut eq = LogiEq::parse(&get_fx(endpoint, LOGI_EQ)?)?;
    for (g, v) in eq.gains.iter_mut().zip(gains) {
        *g = *v;
    }
    set_fx(endpoint, LOGI_EQ, &eq.to_bytes())?;
    Ok(eq)
}

/// The Logitech processing's equaliser, decoded. `rest` keeps whatever
/// follows the 30 numbers, untouched.
#[derive(Debug, Clone, PartialEq)]
pub struct LogiEq {
    pub freqs: [f32; 10],
    pub gains: [f32; 10],
    pub q: [f32; 10],
    pub rest: Vec<u8>,
}

impl LogiEq {
    pub fn parse(b: &[u8]) -> Result<LogiEq> {
        if b.len() < 120 {
            bail!("{} bytes are too few for the equaliser", b.len());
        }
        let f32_at = |o: usize| f32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        let arr = |start: usize| std::array::from_fn(|i| f32_at(start + i * 4));
        Ok(LogiEq { freqs: arr(0), gains: arr(40), q: arr(80), rest: b[120..].to_vec() })
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(120 + self.rest.len());
        for v in self.freqs.iter().chain(&self.gains).chain(&self.q) {
            out.extend(v.to_le_bytes());
        }
        out.extend(&self.rest);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The layout as found, with invented numbers.
    fn sample() -> Vec<u8> {
        let mut b = Vec::new();
        for i in 0..10 {
            b.extend((100.0f32 * (i + 1) as f32).to_le_bytes());
        }
        for i in 0..10 {
            b.extend((i as f32 - 5.0).to_le_bytes());
        }
        for _ in 0..10 {
            b.extend(0.5f32.to_le_bytes());
        }
        b.extend([1, 0, 0, 0, 9, 9, 2, 0, 0, 0]);
        b
    }

    #[test]
    fn the_equaliser_reads_and_writes_back_the_same_bytes() {
        let b = sample();
        let eq = LogiEq::parse(&b).unwrap();
        assert_eq!(eq.freqs[9], 1000.0);
        assert_eq!(eq.gains[0], -5.0);
        assert_eq!(eq.q[3], 0.5);
        assert_eq!(eq.rest, [1, 0, 0, 0, 9, 9, 2, 0, 0, 0]);
        assert_eq!(eq.to_bytes(), b);
    }

    #[test]
    fn too_short_is_an_error() {
        assert!(LogiEq::parse(&[0; 40]).is_err());
    }
}
