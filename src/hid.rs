//! The Windows HID stack, used directly: enumerate HID collections, read their
//! descriptors, and read input reports from them.
//!
//! Every top-level collection of a HID device is its own Windows device with
//! its own path, so "the receiver" is really eight paths (keyboard, consumer
//! keys, two vendor-defined HID++ channels, ...). Collections are always chosen
//! by usage page and usage from their descriptor, never by vendor and product
//! id alone: the G935's dongle has two vendor-defined collections, and only one
//! of them speaks HID++.

use std::ffi::c_void;
use std::sync::mpsc::Sender;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CM_Get_Device_Interface_List_SizeW,
    CM_Get_Device_Interface_ListW, CR_BUFFER_SMALL, CR_SUCCESS,
};
use windows::Win32::Devices::HumanInterfaceDevice::{
    HIDD_ATTRIBUTES, HIDP_CAPS, HIDP_STATUS_SUCCESS, HidD_FreePreparsedData, HidD_GetAttributes,
    HidD_GetHidGuid, HidD_GetPreparsedData, HidD_GetProductString, HidD_SetNumInputBuffers,
    HidP_GetCaps, PHIDP_PREPARSED_DATA,
};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_IO_PENDING, GENERIC_READ, GENERIC_WRITE, HANDLE, WAIT_OBJECT_0,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OVERLAPPED, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING, ReadFile, WriteFile,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Threading::{
    CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
};
use windows::core::PCWSTR;

/// Logitech's USB vendor id.
pub const LOGITECH_VID: u16 = 0x046D;

/// One HID top-level collection, as Windows presents it.
#[derive(Debug, Clone)]
pub struct HidInfo {
    pub path: String,
    pub vid: u16,
    pub pid: u16,
    pub version: u16,
    /// USB interface number (`&MI_xx` in the path), if the device has several.
    pub interface: Option<u8>,
    /// Collection number (`&COLxx`), if the interface has several.
    pub collection: Option<u8>,
    pub usage_page: u16,
    pub usage: u16,
    /// Report lengths in bytes, *including* the report id byte. 0 when the
    /// collection has no report of that kind.
    pub input_len: u16,
    pub output_len: u16,
    pub feature_len: u16,
    pub product: String,
}

impl HidInfo {
    /// Short label like `046D:C547 MI_02 COL01`, for logs.
    pub fn short(&self) -> String {
        let mut s = format!("{:04X}:{:04X}", self.vid, self.pid);
        if let Some(i) = self.interface {
            s.push_str(&format!(" MI_{i:02}"));
        }
        if let Some(c) = self.collection {
            s.push_str(&format!(" COL{c:02}"));
        }
        s
    }
}

/// An owned Win32 handle, closed on drop.
///
/// `Send` because a file handle is just a kernel object reference: using it
/// from another thread is how overlapped reads are meant to be done.
pub struct OwnedHandle(pub HANDLE);

unsafe impl Send for OwnedHandle {}
unsafe impl Sync for OwnedHandle {}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Paths of every present HID collection.
pub fn hid_paths() -> Result<Vec<String>> {
    let guid = unsafe { HidD_GetHidGuid() };
    // The list can grow between the size query and the fetch (a device
    // arriving), which the API reports as CR_BUFFER_SMALL: just ask again.
    for _ in 0..5 {
        let mut len = 0u32;
        let cr = unsafe {
            CM_Get_Device_Interface_List_SizeW(
                &mut len,
                &guid,
                PCWSTR::null(),
                CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
            )
        };
        if cr != CR_SUCCESS {
            bail!("CM_Get_Device_Interface_List_SizeW failed: {:?}", cr);
        }
        let mut buf = vec![0u16; len as usize];
        let cr = unsafe {
            CM_Get_Device_Interface_ListW(
                &guid,
                PCWSTR::null(),
                &mut buf,
                CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
            )
        };
        if cr == CR_BUFFER_SMALL {
            continue;
        }
        if cr != CR_SUCCESS {
            bail!("CM_Get_Device_Interface_ListW failed: {:?}", cr);
        }
        // A sequence of NUL-terminated strings, ending with an empty one.
        return Ok(buf
            .split(|&c| c == 0)
            .filter(|s| !s.is_empty())
            .map(String::from_utf16_lossy)
            .collect());
    }
    bail!("the HID device list kept changing while it was being read")
}

/// Parse `&MI_02` / `&COL01` out of a device path (case-insensitive).
fn path_number(path: &str, tag: &str) -> Option<u8> {
    let upper = path.to_ascii_uppercase();
    let at = upper.find(tag)? + tag.len();
    let digits: String = upper[at..].chars().take_while(|c| c.is_ascii_hexdigit()).collect();
    // MI_ is hex, COL is decimal; both are two digits in practice, where
    // the two readings agree below 10, and Windows writes both zero-padded.
    if tag == "&COL" { digits.parse().ok() } else { u8::from_str_radix(&digits, 16).ok() }
}

/// Open a collection with **no** access rights. Enough to read its
/// attributes and descriptor, and it works even on collections Windows holds
/// exclusively (keyboards, mice), because nothing is being read or written.
pub fn open_query(path: &str) -> Result<OwnedHandle> {
    open(path, 0, FILE_FLAGS_AND_ATTRIBUTES(0))
}

/// Open a collection for overlapped reading only. Shared, so G HUB or any
/// other program can keep its own handle open alongside: HIDClass gives every
/// open handle its own copy of each input report.
pub fn open_read(path: &str) -> Result<OwnedHandle> {
    open(path, GENERIC_READ.0, FILE_FLAG_OVERLAPPED)
}

/// Open a collection for overlapped reading and writing (shared). Only
/// HID++ collections are ever opened this way.
pub fn open_rw(path: &str) -> Result<OwnedHandle> {
    open(path, GENERIC_READ.0 | GENERIC_WRITE.0, FILE_FLAG_OVERLAPPED)
}

/// Send one output report. `report_len` must be the collection's
/// `OutputReportByteLength`: Windows takes exactly that many bytes, so a
/// shorter report is zero-padded here — but a longer one is refused rather
/// than truncated, because a HID++ report sent to the wrong collection must
/// fail loudly, not arrive mangled.
pub fn write_report(handle: &OwnedHandle, data: &[u8], report_len: usize) -> Result<()> {
    if data.len() > report_len {
        bail!("report of {} bytes does not fit a {report_len}-byte collection", data.len());
    }
    let mut buf = vec![0u8; report_len];
    buf[..data.len()].copy_from_slice(data);
    let event = OwnedHandle(
        unsafe { CreateEventW(None, true, false, PCWSTR::null()) }
            .context("creating the write event")?,
    );
    let mut ov = OVERLAPPED { hEvent: event.0, ..Default::default() };
    let started = unsafe { WriteFile(handle.0, Some(&buf), None, Some(&mut ov)) };
    if let Err(e) = started
        && e.code() != ERROR_IO_PENDING.to_hresult()
    {
        bail!("WriteFile: {e}");
    }
    // A HID write completes in a few ms; one that has not after a second is
    // stuck (device gone mid-write). Cancel it, then wait for the cancellation
    // so the kernel is done with `buf` and `ov` before they are freed.
    let waited = unsafe { WaitForSingleObject(event.0, 1000) };
    let mut n = 0u32;
    if waited != WAIT_OBJECT_0 {
        unsafe {
            let _ = CancelIoEx(handle.0, Some(&ov));
            let _ = GetOverlappedResult(handle.0, &ov, &mut n, true);
        }
        bail!("the device did not accept the report within 1 s");
    }
    unsafe { GetOverlappedResult(handle.0, &ov, &mut n, false) }.context("write failed")?;
    Ok(())
}

fn open(path: &str, access: u32, flags: FILE_FLAGS_AND_ATTRIBUTES) -> Result<OwnedHandle> {
    let w = wide(path);
    let h = unsafe {
        CreateFileW(
            PCWSTR(w.as_ptr()),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            flags,
            None,
        )
    }
    .with_context(|| format!("opening {path}"))?;
    Ok(OwnedHandle(h))
}

/// Attributes, descriptor capabilities and product string of one collection.
pub fn query(path: &str) -> Result<HidInfo> {
    let h = open_query(path)?;
    let mut attrs = HIDD_ATTRIBUTES {
        Size: std::mem::size_of::<HIDD_ATTRIBUTES>() as u32,
        ..Default::default()
    };
    if !unsafe { HidD_GetAttributes(h.0, &mut attrs) } {
        bail!("HidD_GetAttributes failed for {path}");
    }
    let mut pp = PHIDP_PREPARSED_DATA::default();
    if !unsafe { HidD_GetPreparsedData(h.0, &mut pp) } {
        bail!("HidD_GetPreparsedData failed for {path}");
    }
    let mut caps = HIDP_CAPS::default();
    let status = unsafe { HidP_GetCaps(pp, &mut caps) };
    unsafe {
        let _ = HidD_FreePreparsedData(pp);
    }
    if status != HIDP_STATUS_SUCCESS {
        bail!("HidP_GetCaps failed for {path}: {:?}", status);
    }
    // 126 wide chars is the documented maximum for a USB string.
    let mut name = [0u16; 128];
    let product = if unsafe {
        HidD_GetProductString(h.0, name.as_mut_ptr() as *mut c_void, (name.len() * 2) as u32)
    } {
        let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
        String::from_utf16_lossy(&name[..end])
    } else {
        String::new()
    };
    Ok(HidInfo {
        path: path.to_string(),
        vid: attrs.VendorID,
        pid: attrs.ProductID,
        version: attrs.VersionNumber,
        interface: path_number(path, "&MI_"),
        collection: path_number(path, "&COL"),
        usage_page: caps.UsagePage,
        usage: caps.Usage,
        input_len: caps.InputReportByteLength,
        output_len: caps.OutputReportByteLength,
        feature_len: caps.FeatureReportByteLength,
        product,
    })
}

/// Every present collection of a Logitech device, with its descriptor
/// capabilities. Paths that cannot be queried (rare: a device mid-removal)
/// are skipped.
pub fn logitech_collections() -> Result<Vec<HidInfo>> {
    let mut out: Vec<HidInfo> = hid_paths()?
        .into_iter()
        .filter(|p| p.to_ascii_uppercase().contains("VID_046D"))
        .filter_map(|p| query(&p).ok())
        .filter(|i| i.vid == LOGITECH_VID)
        .collect();
    out.sort_by_key(|i| (i.pid, i.interface, i.collection));
    Ok(out)
}

/// A manual-reset event used to stop reader threads.
pub struct StopEvent(OwnedHandle);

impl StopEvent {
    pub fn new() -> Result<Self> {
        let h = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }
            .context("creating the stop event")?;
        Ok(Self(OwnedHandle(h)))
    }

    pub fn set(&self) {
        unsafe {
            let _ = SetEvent(self.0.0);
        }
    }

    pub fn raw(&self) -> HANDLE {
        self.0.0
    }
}

/// One input report, as read from a collection.
pub struct Report {
    /// Index of the endpoint in the caller's list.
    pub endpoint: usize,
    pub at: Instant,
    pub data: Vec<u8>,
}

/// Why a reader stopped.
pub enum ReaderEnd {
    Stopped,
    /// The device went away (unplugged, or its driver was restarted).
    Gone(String),
}

/// Read input reports from `handle` until `stop` is set or the device goes
/// away, sending each to `tx`. Blocks; run it on its own thread.
///
/// `report_len` must be the collection's `InputReportByteLength`: Windows
/// rejects reads into a smaller buffer.
pub fn read_reports(
    endpoint: usize,
    handle: &OwnedHandle,
    report_len: usize,
    stop: HANDLE,
    tx: &Sender<Report>,
) -> ReaderEnd {
    // Let reports queue up while this thread is busy forwarding the previous
    // one. The default is 32, which a burst of notifications can overflow.
    unsafe {
        let _ = HidD_SetNumInputBuffers(handle.0, 512);
    }
    let event = match unsafe { CreateEventW(None, true, false, PCWSTR::null()) } {
        Ok(e) => OwnedHandle(e),
        Err(e) => return ReaderEnd::Gone(format!("creating the read event: {e}")),
    };
    let mut buf = vec![0u8; report_len.max(1)];
    loop {
        let mut ov = OVERLAPPED { hEvent: event.0, ..Default::default() };
        let started = unsafe { ReadFile(handle.0, Some(&mut buf), None, Some(&mut ov)) };
        if let Err(e) = started
            && e.code() != ERROR_IO_PENDING.to_hresult()
        {
            return ReaderEnd::Gone(format!("ReadFile: {e}"));
        }
        let waited = unsafe { WaitForMultipleObjects(&[event.0, stop], false, INFINITE) };
        if waited != WAIT_OBJECT_0 {
            // Stop requested (or the wait itself failed): cancel the pending
            // read and wait for the cancellation to land before `ov` and
            // `buf` go out of scope; the kernel still holds pointers to both.
            unsafe {
                let _ = CancelIoEx(handle.0, Some(&ov));
                let mut n = 0u32;
                let _ = GetOverlappedResult(handle.0, &ov, &mut n, true);
            }
            return ReaderEnd::Stopped;
        }
        let mut n = 0u32;
        if let Err(e) = unsafe { GetOverlappedResult(handle.0, &ov, &mut n, false) } {
            return ReaderEnd::Gone(format!("read failed: {e}"));
        }
        let report = Report { endpoint, at: Instant::now(), data: buf[..n as usize].to_vec() };
        if tx.send(report).is_err() {
            return ReaderEnd::Stopped;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interface_and_collection_numbers_come_out_of_the_path() {
        let p = r"\\?\HID#VID_046D&PID_C547&MI_02&Col02#7&9c8d7e6f&0&0001#{4d1e55b2-f16f-11cf-88cb-001111000030}";
        assert_eq!(path_number(p, "&MI_"), Some(2));
        assert_eq!(path_number(p, "&COL"), Some(2));
        let single =
            r"\\?\HID#VID_046D&PID_C232#1&5e6f7a8b&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}";
        assert_eq!(path_number(single, "&MI_"), None);
        assert_eq!(path_number(single, "&COL"), None);
    }
}
