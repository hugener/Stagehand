//! Showing a pen in place of the mouse pointer while the mouse is a laser.
//!
//! There is no way to do this for the overlay alone: the overlay lets input through, so
//! the pointer belongs to whatever is underneath, and only the system-wide shapes can be
//! changed. That makes putting them back non-optional, which is why this is undone when
//! the value is dropped as well as on every path that stops drawing.
//!
//! None of that runs when Stagehand is ended from outside while the pen is showing, by
//! Task Manager or a crash, and the pen would then stay until the user signs out. So a
//! marker file is left for as long as the pen is showing, and a start that finds one
//! puts the pointer back first.

use std::fs;
use std::path::PathBuf;

use windows::Win32::UI::WindowsAndMessaging::{
    CopyIcon, CreateIconFromResourceEx, GetSystemMetrics, HCURSOR, LR_DEFAULTSIZE, OCR_CROSS,
    OCR_HAND, OCR_IBEAM, OCR_NORMAL, SM_CXCURSOR, SPI_SETCURSORS, SPIF_SENDCHANGE, SetSystemCursor,
    SystemParametersInfoW,
};

const CURSOR_FILE: &[u8] = include_bytes!("../../assets/laser_pen.cur");

/// The format version every icon and cursor resource since Windows 3.0 carries.
const RESOURCE_VERSION: u32 = 0x0003_0000;

pub struct DrawingCursor {
    cursor: Option<HCURSOR>,
    replaced: bool,
}

/// The file that says the system pointers are replaced right now.
fn marker() -> PathBuf {
    std::env::temp_dir().join("stagehand-laser-pointer-cursor")
}

/// Reloads every system cursor from the user's own settings, which gives back whatever
/// scheme they have rather than what happened to be there before.
fn reload_system_cursors() {
    // SAFETY: this action takes no parameter data.
    let _ = unsafe { SystemParametersInfoW(SPI_SETCURSORS, 0, None, SPIF_SENDCHANGE) };
    let _ = fs::remove_file(marker());
}

impl DrawingCursor {
    /// Also puts the pointer back if an earlier run ended without doing so.
    pub fn new() -> Self {
        if marker().exists() {
            log::warn!("the pen pointer was left behind by an earlier run; putting it back");
            reload_system_cursors();
        }
        Self {
            cursor: None,
            replaced: false,
        }
    }

    pub fn set_shown(&mut self, shown: bool) {
        if shown { self.apply() } else { self.restore() }
    }

    fn apply(&mut self) {
        if self.replaced {
            return;
        }
        if self.cursor.is_none() {
            self.cursor = load();
        }
        let Some(cursor) = self.cursor else {
            log::warn!("could not load the laser pointer's pen cursor");
            return;
        };

        // Written first: a marker without a pen costs one needless reload, a pen
        // without a marker is the stuck pointer this is here to prevent.
        let _ = fs::write(marker(), b"");

        // The shapes a pointer is most likely to have while a trail is drawn; the rest
        // are left alone.
        for shape in [OCR_NORMAL, OCR_IBEAM, OCR_CROSS, OCR_HAND] {
            // SAFETY: `cursor` is a cursor this value created and never destroys. The
            // system takes ownership of the cursor it is handed, so each shape gets a
            // copy of its own.
            unsafe {
                if let Ok(copy) = CopyIcon(cursor.into()) {
                    let _ = SetSystemCursor(HCURSOR(copy.0), shape);
                }
            }
        }
        self.replaced = true;
    }

    pub fn restore(&mut self) {
        if !self.replaced {
            return;
        }
        reload_system_cursors();
        self.replaced = false;
    }
}

impl Drop for DrawingCursor {
    fn drop(&mut self) {
        self.restore();
    }
}

fn load() -> Option<HCURSOR> {
    // SAFETY: the call takes no pointers and has no preconditions.
    let wanted = unsafe { GetSystemMetrics(SM_CXCURSOR) };
    let image = cursor_resource(CURSOR_FILE, u32::try_from(wanted).unwrap_or(32))?;
    // SAFETY: `image` is a complete cursor resource, which is checked when it is cut
    // out of the file.
    let cursor =
        unsafe { CreateIconFromResourceEx(&image, false, RESOURCE_VERSION, 0, 0, LR_DEFAULTSIZE) };
    cursor.ok().map(|cursor| HCURSOR(cursor.0))
}

/// Cuts the image closest to `wanted_width` pixels out of a `.cur` file, in the form the
/// system expects a cursor resource in: the hot spot followed by the bitmap.
fn cursor_resource(file: &[u8], wanted_width: u32) -> Option<Vec<u8>> {
    let u16_at = |offset: usize| {
        let bytes = file.get(offset..offset + 2)?;
        Some(u16::from_le_bytes([bytes[0], bytes[1]]))
    };
    let u32_at = |offset: usize| {
        let bytes = file.get(offset..offset + 4)?;
        Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    };

    const HEADER_SIZE: usize = 6;
    const ENTRY_SIZE: usize = 16;
    const CURSOR_TYPE: u16 = 2;
    if u16_at(2)? != CURSOR_TYPE {
        return None;
    }

    let entry = (0..usize::from(u16_at(4)?))
        .map(|index| HEADER_SIZE + index * ENTRY_SIZE)
        .min_by_key(|&entry| {
            // A width of zero in the file stands for 256.
            let width = match file.get(entry).copied() {
                Some(0) | None => 256,
                Some(width) => u32::from(width),
            };
            width.abs_diff(wanted_width)
        })?;

    let hot_spot = file.get(entry + 4..entry + 8)?;
    let size = usize::try_from(u32_at(entry + 8)?).ok()?;
    let offset = usize::try_from(u32_at(entry + 12)?).ok()?;
    let bitmap = file.get(offset..offset.checked_add(size)?)?;

    let mut resource = Vec::with_capacity(hot_spot.len() + bitmap.len());
    resource.extend_from_slice(hot_spot);
    resource.extend_from_slice(bitmap);
    Some(resource)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundled_cursor_has_an_image_for_each_size() {
        let small = cursor_resource(CURSOR_FILE, 32).unwrap();
        let large = cursor_resource(CURSOR_FILE, 64).unwrap();

        assert!(large.len() > small.len());
        // The pen's tip, which is where it points, is two pixels in from the corner of
        // the smallest image.
        assert_eq!(&small[..4], &[2, 0, 2, 0]);
    }

    #[test]
    fn a_file_that_is_not_a_cursor_is_refused() {
        assert_eq!(cursor_resource(&[], 32), None);
        assert_eq!(cursor_resource(&[0, 0, 1, 0, 1, 0], 32), None);
        // Claims one image but ends before describing it.
        assert_eq!(cursor_resource(&[0, 0, 2, 0, 1, 0], 32), None);
    }

    #[test]
    fn the_bundled_cursor_can_be_loaded() {
        assert!(load().is_some());
    }
}
