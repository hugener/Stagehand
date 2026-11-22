//! Reading the pen straight from its digitizer.
//!
//! Raw input sees the digitizer directly, so it keeps working over content that takes
//! pointer input for itself, and it reports the pen while it only hovers, before the
//! first contact.

use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::offset_of;

use windows::Win32::Devices::HumanInterfaceDevice::{
    HIDP_CAPS, HIDP_STATUS_SUCCESS, HIDP_VALUE_CAPS, HidP_GetCaps, HidP_GetUsageValue,
    HidP_GetUsages, HidP_GetValueCaps, HidP_Input, PHIDP_PREPARSED_DATA,
};
use windows::Win32::Foundation::{HANDLE, HWND, POINT};
use windows::Win32::UI::Input::{
    GetRawInputData, GetRawInputDeviceInfoW, HRAWINPUT, RAWHID, RAWINPUT, RAWINPUTDEVICE,
    RAWINPUTHEADER, RID_INPUT, RIDEV_INPUTSINK, RIDEV_REMOVE, RIDI_PREPARSEDDATA, RIM_TYPEHID,
    RegisterRawInputDevices,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

const USAGE_PAGE_GENERIC: u16 = 0x01;
const USAGE_GENERIC_X: u16 = 0x30;
const USAGE_GENERIC_Y: u16 = 0x31;
const USAGE_PAGE_DIGITIZER: u16 = 0x0D;
const USAGE_DIGITIZER_PEN: u16 = 0x02;
const USAGE_DIGITIZER_TIP_SWITCH: u16 = 0x42;

/// One report from the pen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PenReport {
    /// Where the pen is, in screen pixels.
    pub point: POINT,
    /// Whether the tip is touching the screen, as opposed to hovering over it.
    pub tip_down: bool,
}

/// How one digitizer describes itself. Kept per device because asking for it on every
/// report would cost more than reading the report.
#[derive(Default)]
struct Device {
    // Words rather than bytes so the description is aligned the way its reader expects.
    description: Vec<u64>,
    x: Option<(i32, i32)>,
    y: Option<(i32, i32)>,
}

#[derive(Default)]
pub struct PenInput {
    registered: bool,
    devices: HashMap<isize, Device>,
    // Words rather than bytes so a report can be read in place.
    buffer: Vec<u64>,
}

impl PenInput {
    /// Asks for pen reports to be delivered to `window` even while another application
    /// is in front, which is where a pen is normally used.
    pub fn register(&mut self, window: HWND) {
        if self.registered {
            return;
        }
        let device = RAWINPUTDEVICE {
            usUsagePage: USAGE_PAGE_DIGITIZER,
            usUsage: USAGE_DIGITIZER_PEN,
            dwFlags: RIDEV_INPUTSINK,
            hwndTarget: window,
        };
        // SAFETY: `window` belongs to this thread and outlives the registration, which
        // `unregister` removes before the window is destroyed.
        match unsafe { RegisterRawInputDevices(&[device], size_of::<RAWINPUTDEVICE>() as u32) } {
            Ok(()) => self.registered = true,
            Err(error) => log::error!("could not listen to the pen: {error}"),
        }
    }

    pub fn unregister(&mut self) {
        if !self.registered {
            return;
        }
        let device = RAWINPUTDEVICE {
            usUsagePage: USAGE_PAGE_DIGITIZER,
            usUsage: USAGE_DIGITIZER_PEN,
            dwFlags: RIDEV_REMOVE,
            hwndTarget: HWND::default(),
        };
        // SAFETY: removing a registration takes no window.
        let _ = unsafe { RegisterRawInputDevices(&[device], size_of::<RAWINPUTDEVICE>() as u32) };
        self.registered = false;
        self.devices.clear();
    }

    /// Reads the report a raw input message refers to. `None` when it is not from a pen
    /// this can make sense of.
    pub fn read(&mut self, input: HRAWINPUT) -> Option<PenReport> {
        let header_size = size_of::<RAWINPUTHEADER>() as u32;
        let mut size = 0u32;
        // SAFETY: with no buffer the call only reports the size needed.
        if unsafe { GetRawInputData(input, RID_INPUT, None, &mut size, header_size) } != 0
            || (size as usize) < size_of::<RAWINPUT>()
        {
            return None;
        }

        let words = (size as usize).div_ceil(size_of::<u64>());
        if self.buffer.len() < words {
            self.buffer.resize(words, 0);
        }
        let buffer: *mut c_void = self.buffer.as_mut_ptr().cast();
        // SAFETY: the buffer holds at least `size` bytes, as made sure of just above.
        if unsafe { GetRawInputData(input, RID_INPUT, Some(buffer), &mut size, header_size) }
            != size
        {
            return None;
        }

        // SAFETY: the buffer was just filled with a RAWINPUT, is at least as large as
        // one, and is aligned for it because it is made of 64-bit words.
        let header = unsafe { (*buffer.cast::<RAWINPUT>()).header };
        if header.dwType != RIM_TYPEHID.0 {
            return None;
        }
        // SAFETY: the type says this is a report from a HID device, so `hid` is the
        // part of the union that was filled in. Only its two counts are copied out.
        let (report_size, report_count) = unsafe {
            let hid = &(*buffer.cast::<RAWINPUT>()).data.hid;
            (hid.dwSizeHid as usize, hid.dwCount)
        };
        let report_offset = offset_of!(RAWINPUT, data) + offset_of!(RAWHID, bRawData);
        if report_count == 0 || report_offset + report_size > size as usize {
            return None;
        }
        // SAFETY: the first report starts at `bRawData` and is `dwSizeHid` bytes long,
        // which was just checked to lie inside what the system wrote. Nothing else
        // refers to the buffer while this slice is alive.
        let report = unsafe {
            std::slice::from_raw_parts_mut(buffer.cast::<u8>().add(report_offset), report_size)
        };

        let device = self
            .devices
            .entry(header.hDevice.0 as isize)
            .or_insert_with(|| describe(header.hDevice));
        device.read(report)
    }
}

impl Device {
    fn description(&self) -> PHIDP_PREPARSED_DATA {
        PHIDP_PREPARSED_DATA(self.description.as_ptr() as isize)
    }

    fn read(&self, report: &mut [u8]) -> Option<PenReport> {
        let (x_range, y_range) = (self.x?, self.y?);
        let value = |usage: u16| {
            let mut value = 0u32;
            // SAFETY: the description is the one the system gave for the device the
            // report came from, and `value` is a live local.
            let status = unsafe {
                HidP_GetUsageValue(
                    HidP_Input,
                    USAGE_PAGE_GENERIC,
                    None,
                    usage,
                    &mut value,
                    self.description(),
                    report,
                )
            };
            (status == HIDP_STATUS_SUCCESS).then_some(value)
        };
        let x = value(USAGE_GENERIC_X)?;
        let y = value(USAGE_GENERIC_Y)?;

        // The tip switch tells contact from hover. A pen without one simply never reads
        // as touching, which only matters when hovering is not enough to draw.
        let mut usages = [0u16; 16];
        let mut count = usages.len() as u32;
        // SAFETY: `usages` has room for `count` entries, and the description belongs to
        // the device the report came from.
        let status = unsafe {
            HidP_GetUsages(
                HidP_Input,
                USAGE_PAGE_DIGITIZER,
                None,
                usages.as_mut_ptr(),
                &mut count,
                self.description(),
                report,
            )
        };
        let tip_down = status == HIDP_STATUS_SUCCESS
            && usages[..(count as usize).min(usages.len())].contains(&USAGE_DIGITIZER_TIP_SWITCH);

        // The digitizer counts in its own units across its whole surface; that surface
        // is laid over the whole desktop.
        // SAFETY: the calls take no pointers and have no preconditions.
        let (left, top, width, height) = unsafe {
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
            )
        };
        Some(PenReport {
            point: POINT {
                x: to_screen(x, x_range, left, width)?,
                y: to_screen(y, y_range, top, height)?,
            },
            tip_down,
        })
    }
}

/// Maps a digitizer reading onto one axis of the desktop.
fn to_screen(value: u32, (low, high): (i32, i32), origin: i32, extent: i32) -> Option<i32> {
    let span = f64::from(high) - f64::from(low);
    if span <= 0.0 {
        return None;
    }
    let fraction = (f64::from(value) - f64::from(low)) / span;
    Some(origin + (fraction * f64::from(extent)) as i32)
}

/// Asks the system how a digitizer lays out its reports. A device that cannot be read
/// is remembered as such, so it is not asked again on every report.
fn describe(handle: HANDLE) -> Device {
    let mut device = Device::default();

    let mut size = 0u32;
    // SAFETY: with no buffer the call only reports the size needed.
    let result =
        unsafe { GetRawInputDeviceInfoW(Some(handle), RIDI_PREPARSEDDATA, None, &mut size) };
    if result != 0 || size == 0 {
        return device;
    }
    device
        .description
        .resize((size as usize).div_ceil(size_of::<u64>()), 0);
    let buffer: *mut c_void = device.description.as_mut_ptr().cast();
    // SAFETY: the buffer holds at least `size` bytes.
    let result = unsafe {
        GetRawInputDeviceInfoW(Some(handle), RIDI_PREPARSEDDATA, Some(buffer), &mut size)
    };
    if result == u32::MAX {
        return device;
    }

    let mut capabilities = HIDP_CAPS::default();
    // SAFETY: the description was just filled in by the system, and `capabilities` is
    // a live local.
    if unsafe { HidP_GetCaps(device.description(), &mut capabilities) } != HIDP_STATUS_SUCCESS {
        return device;
    }

    let mut count = capabilities.NumberInputValueCaps;
    let mut values = vec![HIDP_VALUE_CAPS::default(); usize::from(count)];
    // SAFETY: `values` has room for `count` entries, which is how many the device has.
    let status = unsafe {
        HidP_GetValueCaps(
            HidP_Input,
            values.as_mut_ptr(),
            &mut count,
            device.description(),
        )
    };
    if count == 0 || status != HIDP_STATUS_SUCCESS {
        return device;
    }

    for value in &values[..usize::from(count).min(values.len())] {
        if value.UsagePage != USAGE_PAGE_GENERIC
            || value.IsRange
            || value.LogicalMax <= value.LogicalMin
        {
            continue;
        }
        let range = Some((value.LogicalMin, value.LogicalMax));
        // SAFETY: `IsRange` is false, so the single-usage part of the union is the one
        // that was filled in.
        match unsafe { value.Anonymous.NotRange.Usage } {
            USAGE_GENERIC_X => device.x = range,
            USAGE_GENERIC_Y => device.y = range,
            _ => {}
        }
    }

    log::info!("pen digitizer found (x: {:?}, y: {:?})", device.x, device.y);
    device
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reading_is_laid_over_the_desktop() {
        // A desktop that starts left of the main display, as with a second monitor.
        assert_eq!(to_screen(0, (0, 20_000), -1920, 3840), Some(-1920));
        assert_eq!(to_screen(10_000, (0, 20_000), -1920, 3840), Some(0));
        assert_eq!(to_screen(20_000, (0, 20_000), -1920, 3840), Some(1920));
        assert_eq!(to_screen(150, (100, 200), 0, 1000), Some(500));
    }

    #[test]
    fn a_digitizer_without_a_range_gives_no_position() {
        assert_eq!(to_screen(5, (10, 10), 0, 1000), None);
        assert_eq!(to_screen(5, (10, 0), 0, 1000), None);
    }
}
