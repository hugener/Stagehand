//! Reading and moving the Windows taskbar.
//!
//! The Windows Settings app moves the taskbar through a setting handler, and so does
//! this. On Windows 11 the docking edge is owned by the shell: writing the registry
//! values older versions read no longer moves the taskbar, so the handler is the only
//! way to move it without signing out.

use std::ffi::c_void;

use windows::Foundation::{IReference, PropertyValue};
use windows::Win32::System::WinRT::RoActivateInstance;
use windows::Win32::UI::Shell::{
    ABE_BOTTOM, ABE_LEFT, ABE_RIGHT, ABE_TOP, ABM_GETTASKBARPOS, APPBARDATA, SHAppBarMessage,
};
use windows::core::{GUID, HRESULT, HSTRING, IInspectable, IUnknown, Interface, Result, h};

use crate::settings::TaskbarEdge;

const SETTING_CLASS: &str = "SystemSettings.Desktop.Taskbar.DesktopTaskbarLoSetting";
const ACTIVATABLE_CLASSES_KEY: &str = r"SOFTWARE\Microsoft\WindowsRuntime\ActivatableClassId";

/// Interface id of `SystemSettings.DataModel.ISettingItem`. Windows does not publish
/// the interface, so its layout is spelled out below.
const SETTING_ITEM_IID: GUID = GUID::from_u128(0x40C037CC_D8BF_489E_8697_D66BAA3221BF);

/// The start of the method table of `ISettingItem`, up to the last method used here.
#[repr(C)]
struct SettingItemMethods {
    /// QueryInterface, AddRef, Release, GetIids, GetRuntimeClassName, GetTrustLevel.
    inspectable: [usize; 6],
    /// get_Id, get_SettingType, get_IsSetByGroupPolicy, get_IsEnabled,
    /// get_IsApplicable, get_Description, get_IsUpdating. Never called; they are here
    /// so the two below sit in the right slots.
    unused: [usize; 7],
    get_value: unsafe extern "system" fn(
        this: *mut c_void,
        name: *mut c_void,
        value: *mut *mut c_void,
    ) -> HRESULT,
    set_value: unsafe extern "system" fn(
        this: *mut c_void,
        name: *mut c_void,
        value: *mut c_void,
    ) -> HRESULT,
}

/// A setting from the Windows Settings app. Holds an `ISettingItem` pointer, which is
/// released when this is dropped.
struct SettingItem(IUnknown);

impl SettingItem {
    fn activate(class: &str) -> Result<Self> {
        // SAFETY: the class name is a valid string for the duration of the call.
        let instance = unsafe { RoActivateInstance(&HSTRING::from(class)) }?;
        let mut item = std::ptr::null_mut();
        // SAFETY: `item` is a valid place for the interface pointer.
        unsafe { instance.query(&SETTING_ITEM_IID, &mut item) }.ok()?;
        // SAFETY: the query succeeded, so `item` is an ISettingItem pointer whose
        // reference is now owned here. Every interface starts with IUnknown.
        Ok(Self(unsafe { IUnknown::from_raw(item) }))
    }

    fn methods(&self) -> &SettingItemMethods {
        // SAFETY: the pointer came from a successful query for ISettingItem, so it
        // points to a pointer to that interface's method table, which lives as long as
        // the object does.
        unsafe { &**self.0.as_raw().cast::<*const SettingItemMethods>() }
    }

    fn value(&self, name: &HSTRING) -> Result<Option<IInspectable>> {
        let mut value = std::ptr::null_mut();
        // SAFETY: the method belongs to this object, the name is a live string handle,
        // and `value` is a valid place for the returned object.
        unsafe { (self.methods().get_value)(self.0.as_raw(), string_handle(name), &mut value) }
            .ok()?;
        if value.is_null() {
            return Ok(None);
        }
        // SAFETY: the call succeeded and returned an object, whose reference is now
        // owned here.
        Ok(Some(unsafe { IInspectable::from_raw(value) }))
    }

    fn set_value(&self, name: &HSTRING, value: &IInspectable) -> Result<()> {
        // SAFETY: the method belongs to this object, and the name and the value stay
        // alive for the duration of the call.
        unsafe { (self.methods().set_value)(self.0.as_raw(), string_handle(name), value.as_raw()) }
            .ok()
    }
}

/// Whether this version of Windows has the setting the taskbar is moved through.
///
/// This only looks the setting up; it does not load it, so asking is cheap and safe from
/// any thread.
pub fn is_available() -> bool {
    windows_registry::LOCAL_MACHINE
        .open(format!(r"{ACTIVATABLE_CLASSES_KEY}\{SETTING_CLASS}"))
        .is_ok()
}

/// The edge the primary taskbar is docked on, or `None` when Windows does not say.
///
/// Must be called on a thread that has initialised the Windows Runtime.
pub fn current() -> Option<TaskbarEdge> {
    current_from_setting()
        .ok()
        .flatten()
        .or_else(current_from_app_bar)
}

/// Docks the taskbar on `edge`. Returns whether it had to be moved.
///
/// Must be called on a thread that has initialised the Windows Runtime.
pub fn apply(edge: TaskbarEdge) -> Result<bool> {
    if current() == Some(edge) {
        return Ok(false);
    }
    set_through_setting(edge)?;
    Ok(true)
}

fn current_from_setting() -> Result<Option<TaskbarEdge>> {
    let setting = SettingItem::activate(SETTING_CLASS)?;
    let Some(value) = setting.value(h!("Value"))? else {
        return Ok(None);
    };
    let number = value.cast::<IReference<i32>>()?.Value()?;
    Ok(TaskbarEdge::from_shell_value(number))
}

fn set_through_setting(edge: TaskbarEdge) -> Result<()> {
    let setting = SettingItem::activate(SETTING_CLASS)?;
    // The number comes from `TaskbarEdge`, so it is one the setting accepts; anything
    // else would crash this process.
    let value = PropertyValue::CreateInt32(edge.shell_value())?;
    setting.set_value(h!("Value"), &value)
}

/// The handle a Windows Runtime method takes for a string parameter.
fn string_handle(string: &HSTRING) -> *mut c_void {
    // SAFETY: HSTRING is a transparent wrapper around the handle, which is a pointer.
    // The copy is only borrowed: the caller keeps `string` alive while it is in use.
    unsafe { std::mem::transmute_copy(string) }
}

/// What the taskbar itself reports. The numbering differs from the setting's.
fn current_from_app_bar() -> Option<TaskbarEdge> {
    let mut data = APPBARDATA {
        cbSize: size_of::<APPBARDATA>() as u32,
        ..Default::default()
    };
    // SAFETY: `data` is a correctly sized APPBARDATA that outlives the call.
    if unsafe { SHAppBarMessage(ABM_GETTASKBARPOS, &mut data) } == 0 {
        return None;
    }
    match data.uEdge {
        ABE_LEFT => Some(TaskbarEdge::Left),
        ABE_TOP => Some(TaskbarEdge::Top),
        ABE_RIGHT => Some(TaskbarEdge::Right),
        ABE_BOTTOM => Some(TaskbarEdge::Bottom),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};

    use super::*;

    fn initialise_runtime() {
        // SAFETY: initialising the Windows Runtime on a test thread has no
        // preconditions. It is left initialised; the thread ends with the test.
        let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
    }

    #[test]
    fn the_setting_reports_the_same_edge_as_the_taskbar() {
        if !is_available() {
            // Windows Server, which the tests also run on, has no such setting.
            return;
        }
        initialise_runtime();

        let from_setting = current_from_setting().unwrap();

        assert!(from_setting.is_some());
        if let Some(from_app_bar) = current_from_app_bar() {
            assert_eq!(from_setting, Some(from_app_bar));
        }
    }

    /// Writes the edge the taskbar is already on, so nothing moves, but the whole write
    /// path runs. Not part of the normal run because it touches the user's desktop.
    #[test]
    #[ignore = "writes the taskbar setting on this machine"]
    fn writing_the_current_edge_succeeds() {
        initialise_runtime();
        let edge = current_from_setting().unwrap().unwrap();

        set_through_setting(edge).unwrap();

        assert_eq!(current_from_setting().unwrap(), Some(edge));
    }
}
