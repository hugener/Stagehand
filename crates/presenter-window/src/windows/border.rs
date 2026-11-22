//! Marking the window that is being mirrored, the way a conferencing app marks the
//! window it shares: an outline for as long as it lasts, and its name for the first
//! moments, so that a wrong pick is obvious at once instead of being found out later.
//!
//! The outline is a window of its own, cut down to a frame, that sits right in front of
//! the mirrored window. It is on this machine only and never part of what viewers see.

use std::cell::RefCell;

use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BLACK_BRUSH, BeginPaint, CLEARTYPE_QUALITY, CombineRgn, CreateFontW, CreateRectRgn,
    CreateSolidBrush, DEFAULT_CHARSET, DT_CALCRECT, DT_END_ELLIPSIS, DT_NOPREFIX, DT_SINGLELINE,
    DT_VCENTER, DeleteObject, DrawTextW, EndPaint, FF_DONTCARE, FW_SEMIBOLD, FillRect, GetDC,
    GetStockObject, HBRUSH, HFONT, InvalidateRect, OUT_DEFAULT_PRECIS, PAINTSTRUCT, RGN_DIFF,
    RGN_OR, ReleaseDC, SelectObject, SetBkMode, SetTextColor, SetWindowRgn, TRANSPARENT,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GWL_EXSTYLE, GetClientRect, GetWindowLongPtrW,
    HWND_TOP, HWND_TOPMOST, LWA_ALPHA, RegisterClassExW, SW_HIDE, SW_SHOWNOACTIVATE,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetLayeredWindowAttributes, SetWindowPos,
    ShowWindow, UnregisterClassW, WM_ERASEBKGND, WM_PAINT, WNDCLASSEXW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{PCWSTR, Result, w};

use super::target;

const WINDOW_CLASS: PCWSTR = w!("StagehandPresenterBorder");

/// The red Teams outlines a shared window with, so the two do not look like different
/// things happening. Stored the way GDI wants it, as 0x00BBGGRR.
const BORDER_COLOR: COLORREF = COLORREF(0x004B_31C4);
const WHITE: COLORREF = COLORREF(0x00FF_FFFF);
// At 100% scaling; multiplied by the scaling of the monitor the window is on.
const BORDER_THICKNESS: f32 = 5.0;
const LABEL_FONT_HEIGHT: f32 = 20.0;
const LABEL_PADDING: (f32, f32) = (8.0, 4.0);
const LABEL_INSET: (f32, f32) = (6.0, 6.0);

/// The mirrored window can be raised or lowered at any time, but looking through the
/// window order on every frame would be wasteful.
const Z_ORDER_REFRESH_MS: u64 = 400;

struct Label {
    text: Vec<u16>,
    font: HFONT,
    // Relative to the border window.
    rect: RECT,
    padding: (i32, i32),
}

thread_local! {
    // What the window procedure paints. It is called by Windows with no way to hand it
    // a value, and always on the thread that owns the border.
    static LABEL: RefCell<Option<Label>> = const { RefCell::new(None) };
    static BRUSH: RefCell<Option<HBRUSH>> = const { RefCell::new(None) };
}

pub fn register_class(instance: HINSTANCE) -> Result<()> {
    let class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(window_procedure),
        hInstance: instance,
        lpszClassName: WINDOW_CLASS,
        ..Default::default()
    };
    // SAFETY: `class` is fully initialised and its strings are static.
    if unsafe { RegisterClassExW(&class) } == 0 {
        return Err(windows::core::Error::from_thread());
    }
    Ok(())
}

pub fn unregister_class(instance: HINSTANCE) {
    // SAFETY: called once the class has no windows left.
    let _ = unsafe { UnregisterClassW(WINDOW_CLASS, Some(instance)) };
}

unsafe extern "system" fn window_procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        // Everything is painted in one go below.
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            // SAFETY: the window belongs to this thread, painting is begun and ended
            // right here, and every rectangle and text handed over is a live local or
            // owned by the label being borrowed.
            unsafe {
                let context = BeginPaint(window, &mut paint);
                let mut client = RECT::default();
                let _ = GetClientRect(window, &mut client);
                if let Some(brush) = BRUSH.with_borrow(|brush| *brush) {
                    FillRect(context, &client, brush);
                }
                LABEL.with_borrow_mut(|label| {
                    let Some(label) = label else { return };
                    FillRect(context, &label.rect, HBRUSH(GetStockObject(BLACK_BRUSH).0));
                    let mut text_rect = RECT {
                        left: label.rect.left + label.padding.0,
                        top: label.rect.top + label.padding.1,
                        right: label.rect.right - label.padding.0,
                        bottom: label.rect.bottom - label.padding.1,
                    };
                    SetBkMode(context, TRANSPARENT);
                    SetTextColor(context, WHITE);
                    let previous = SelectObject(context, label.font.into());
                    DrawTextW(
                        context,
                        &mut label.text,
                        &mut text_rect,
                        DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
                    );
                    SelectObject(context, previous);
                });
                let _ = EndPaint(window, &paint);
            }
            LRESULT(0)
        }
        // SAFETY: the arguments are passed on exactly as Windows supplied them.
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

fn drop_label() {
    if let Some(label) = LABEL.take() {
        // SAFETY: the font was made for this label and is no longer selected anywhere.
        let _ = unsafe { DeleteObject(label.font.into()) };
    }
}

pub struct Border {
    window: HWND,
    visible: bool,
    // What the window was last laid out for.
    rect: Option<RECT>,
    label: Option<String>,
    last_z_order_ms: u64,
}

impl Border {
    pub fn create(instance: HINSTANCE) -> Result<Self> {
        // Layered and transparent together is what lets every click through to the
        // window underneath; a tool window stays out of the taskbar.
        let style = WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        // SAFETY: the class is registered by `register_class`, every string is static,
        // and the window belongs to this thread from here on.
        let window = unsafe {
            let window = CreateWindowExW(
                style,
                WINDOW_CLASS,
                w!("Stagehand Presenter Border"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance),
                None,
            )?;
            let _ = SetLayeredWindowAttributes(window, COLORREF(0), 255, LWA_ALPHA);
            BRUSH.set(Some(CreateSolidBrush(BORDER_COLOR)));
            window
        };
        Ok(Self {
            window,
            visible: false,
            rect: None,
            label: None,
            last_z_order_ms: 0,
        })
    }

    pub fn hide(&mut self) {
        if self.visible {
            // SAFETY: the window belongs to this thread.
            let _ = unsafe { ShowWindow(self.window, SW_HIDE) };
            self.visible = false;
        }
        self.rect = None;
    }

    /// Puts the outline around `target`, which is at `rect` on screen, and shows `label`
    /// inside its top left corner when there is one. Called on every frame, and only
    /// does something when something changed.
    pub fn update(&mut self, target: HWND, rect: RECT, label: Option<&str>, now_ms: u64) {
        // An outline around a window that is minimised or on another desktop would
        // mark nothing.
        if !target::is_on_screen(target) {
            self.hide();
            return;
        }

        if self.rect != Some(rect) || self.label.as_deref() != label {
            self.lay_out(target, rect, label);
        }

        if !self.visible || now_ms.saturating_sub(self.last_z_order_ms) >= Z_ORDER_REFRESH_MS {
            self.last_z_order_ms = now_ms;
            self.place_in_front_of(target);
        }

        if !self.visible {
            // SAFETY: the window belongs to this thread.
            let _ = unsafe { ShowWindow(self.window, SW_SHOWNOACTIVATE) };
            self.visible = true;
        }
    }

    fn lay_out(&mut self, target: HWND, rect: RECT, label: Option<&str>) {
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        // SAFETY: only reads how the target's monitor is scaled.
        let scale = unsafe { GetDpiForWindow(target) }.max(96) as f32 / 96.0;
        let thickness = (BORDER_THICKNESS * scale).round() as i32;
        let scaled = |value: f32| (value * scale).round() as i32;

        drop_label();
        // SAFETY: the window belongs to this thread; every GDI object made here is
        // either released here or handed to the window or the label, which own it from
        // then on.
        unsafe {
            let _ = SetWindowPos(
                self.window,
                None,
                rect.left,
                rect.top,
                width,
                height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );

            // A frame: the whole window with its middle taken out, so that there is
            // nothing over the mirrored window's content to be in anyone's way.
            let region = CreateRectRgn(0, 0, width, height);
            let hole = CreateRectRgn(thickness, thickness, width - thickness, height - thickness);
            CombineRgn(Some(region), Some(region), Some(hole), RGN_DIFF);
            let _ = DeleteObject(hole.into());

            if let Some(label) = label {
                let font = CreateFontW(
                    -scaled(LABEL_FONT_HEIGHT),
                    0,
                    0,
                    0,
                    FW_SEMIBOLD.0 as i32,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET,
                    OUT_DEFAULT_PRECIS,
                    Default::default(),
                    CLEARTYPE_QUALITY,
                    FF_DONTCARE.0.into(),
                    w!("Segoe UI"),
                );
                let mut text: Vec<u16> = format!("Sharing: {label}").encode_utf16().collect();

                // Measured, so the box is as wide as the name and no wider.
                let context = GetDC(Some(self.window));
                let previous = SelectObject(context, font.into());
                let mut measured = RECT::default();
                DrawTextW(
                    context,
                    &mut text,
                    &mut measured,
                    DT_SINGLELINE | DT_CALCRECT | DT_NOPREFIX,
                );
                SelectObject(context, previous);
                ReleaseDC(Some(self.window), context);

                let padding = (scaled(LABEL_PADDING.0), scaled(LABEL_PADDING.1));
                let left = thickness + scaled(LABEL_INSET.0);
                let top = thickness + scaled(LABEL_INSET.1);
                // A long name is cut short inside the window instead of running out
                // of it.
                let available = (width - thickness - scaled(LABEL_INSET.0) - left).max(0);
                let box_rect = RECT {
                    left,
                    top,
                    right: left + (measured.right + 2 * padding.0).min(available),
                    bottom: top + measured.bottom + 2 * padding.1,
                };
                let label_region =
                    CreateRectRgn(box_rect.left, box_rect.top, box_rect.right, box_rect.bottom);
                CombineRgn(Some(region), Some(region), Some(label_region), RGN_OR);
                let _ = DeleteObject(label_region.into());

                LABEL.set(Some(Label {
                    text,
                    font,
                    rect: box_rect,
                    padding,
                }));
            }

            // The window takes ownership of the region.
            SetWindowRgn(self.window, Some(region), true);
            let _ = InvalidateRect(Some(self.window), None, false);
        }

        self.rect = Some(rect);
        self.label = label.map(str::to_owned);
    }

    /// The outline belongs to the mirrored window, so it should be covered when that
    /// window is. It is therefore kept in that window's own place in the window order,
    /// not on top of everything.
    fn place_in_front_of(&mut self, target: HWND) {
        // Looked up in the window order itself every time, not remembered: the target
        // is raised above the outline whenever the user clicks it, without anything
        // else around it changing.
        let above = target::windows_above(target)
            .map(|window| (window, window == self.window, target::is_own_window(window)));
        let after = match neighbour(above) {
            // Asking for the same place again on a timer makes the outline blink.
            Neighbour::Outline => return,
            Neighbour::Window(window) => window,
            // With nothing in front of it the target is at the front of its band,
            // which is the topmost one for a window that is always on top.
            Neighbour::Nothing => {
                // SAFETY: only reads the style of another window.
                let style = unsafe { GetWindowLongPtrW(target, GWL_EXSTYLE) };
                if style & WS_EX_TOPMOST.0 as isize != 0 {
                    HWND_TOPMOST
                } else {
                    HWND_TOP
                }
            }
        };
        // A window is placed behind the one it is given, so the outline goes behind
        // whatever is right in front of the target.
        // SAFETY: the window belongs to this thread.
        let _ = unsafe {
            SetWindowPos(
                self.window,
                Some(after),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            )
        };
    }
}

/// What is right in front of the mirrored window.
#[derive(Debug, PartialEq)]
enum Neighbour<W> {
    /// The outline, which is then where it belongs.
    Outline,
    Window(W),
    Nothing,
}

/// Finds what is right in front of the mirrored window. `above` is the windows that
/// show on screen in front of it, nearest first, each with whether it is the outline
/// and whether it is one of Stagehand's own. The other windows of Stagehand's own are
/// looked past: the laser's overlay is above everything and covers nothing.
fn neighbour<W>(above: impl IntoIterator<Item = (W, bool, bool)>) -> Neighbour<W> {
    above
        .into_iter()
        .find(|&(_, is_outline, is_own)| is_outline || !is_own)
        .map_or(Neighbour::Nothing, |(window, is_outline, _)| {
            if is_outline {
                Neighbour::Outline
            } else {
                Neighbour::Window(window)
            }
        })
}

impl Drop for Border {
    fn drop(&mut self) {
        // SAFETY: the window was created by this thread and is destroyed once; the
        // brush is no longer used once the window is gone.
        unsafe {
            let _ = DestroyWindow(self.window);
            if let Some(brush) = BRUSH.take() {
                let _ = DeleteObject(brush.into());
            }
        }
        drop_label();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUTLINE: (&str, bool, bool) = ("outline", true, true);

    #[test]
    fn an_outline_right_in_front_stays_where_it_is() {
        let above = [OUTLINE, ("editor", false, false)];

        assert_eq!(neighbour(above), Neighbour::Outline);
    }

    #[test]
    fn an_outline_the_window_was_raised_above_is_put_back() {
        // The user clicked the shared window: it is now in front of its own outline,
        // and nothing shows in front of it any more.
        assert_eq!(neighbour::<&str>([]), Neighbour::Nothing);

        // The same, with another window still in front of both.
        let above = [("call", false, false), OUTLINE];
        assert_eq!(neighbour(above), Neighbour::Window("call"));
    }

    #[test]
    fn the_other_windows_of_stagehand_are_looked_past() {
        let above = [("laser overlay", false, true), OUTLINE];
        assert_eq!(neighbour(above), Neighbour::Outline);

        let above = [("laser overlay", false, true), ("call", false, false)];
        assert_eq!(neighbour(above), Neighbour::Window("call"));

        let above = [("laser overlay", false, true)];
        assert_eq!(neighbour(above), Neighbour::Nothing);
    }
}
