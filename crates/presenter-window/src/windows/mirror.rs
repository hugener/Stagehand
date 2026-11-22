//! The window that can be shared: a copy of the picked window with the tools' drawing
//! on top.
//!
//! It is parked beyond the edge of the desktop so it does not clutter the presenter's
//! screen. Windows keeps composing windows that are off screen, and Teams and OBS still
//! list them for sharing.

use std::ffi::c_void;

use stagehand_core::Shape;
use windows::Graphics::Capture::{
    Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::{HINSTANCE, HMODULE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D1_ALPHA_MODE_IGNORE, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED,
    D2D1_FIGURE_END_CLOSED, D2D1_FILL_MODE_WINDING, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_ALIASED, D2D1_BITMAP_OPTIONS, D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
    D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1,
    D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR, D2D1CreateFactory, ID2D1Bitmap1, ID2D1Brush,
    ID2D1DeviceContext, ID2D1Factory1, ID2D1Image, ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11CreateDevice, ID3D11Device,
    ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_ERROR_UNSUPPORTED, DXGI_MWA_NO_ALT_ENTER, DXGI_PRESENT, DXGI_SWAP_CHAIN_DESC1,
    DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL, DXGI_USAGE_RENDER_TARGET_OUTPUT,
    IDXGIDevice, IDXGIFactory2, IDXGISurface, IDXGISwapChain1,
};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
    DeleteDC, DeleteObject, GetDC, RDW_ALLCHILDREN, RDW_INVALIDATE, RedrawWindow, ReleaseDC,
    SelectObject,
};
use windows::Win32::Storage::Xps::{PRINT_WINDOW_FLAGS, PrintWindow};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA, GetSystemMetrics,
    GetWindowLongPtrW, LoadIconW, PostMessageW, RegisterClassExW, SM_CXVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOZORDER, SetWindowLongPtrW, SetWindowPos, SetWindowTextW, ShowWindow, UnregisterClassW,
    WM_CLOSE, WNDCLASSEXW, WS_EX_APPWINDOW, WS_POPUP,
};
use windows::core::{HSTRING, Interface, PCWSTR, Result, w};
use windows_numerics::Vector2;

use super::target;

const WINDOW_CLASS: PCWSTR = w!("StagehandPresenterWindow");
/// The number the program's icon is built in under; see the application's build script.
const PROGRAM_ICON: usize = 1;

/// How far beyond the right-hand edge of the desktop the mirror is parked, so that no
/// part of it lands on any monitor.
const OFFSCREEN_MARGIN: i32 = 200;
/// How often a window that has not been painted yet is asked to, while waiting for its
/// first picture.
const NUDGE_INTERVAL_MS: u64 = 200;
/// How long to wait for a first picture before asking the window to print itself.
const SEED_AFTER_MS: u64 = 1000;
/// Two is enough: pictures are taken on every frame, and a deeper pool only adds delay
/// between the window changing and the mirror showing it.
const CAPTURE_BUFFERS: i32 = 2;
/// Asks a window to print everything, which is what makes printing work for windows
/// that Windows composes on the graphics card.
const PW_RENDERFULLCONTENT: PRINT_WINDOW_FLAGS = PRINT_WINDOW_FLAGS(2);

pub fn register_class(instance: HINSTANCE) -> Result<()> {
    // The icon built into the program, which viewers see beside the mirror in a call's
    // list of windows. A build without one, such as a test, gets the system's default.
    // SAFETY: a resource is named by its number in place of a string, which is what the
    // small integer stands for; nothing is read through it.
    let icon = unsafe { LoadIconW(Some(instance), PCWSTR(PROGRAM_ICON as *const u16)) };
    let class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(window_procedure),
        hInstance: instance,
        hIcon: icon.unwrap_or_default(),
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

/// The message a mirror posts to the window it was given when the user closes it.
pub const WM_MIRROR_CLOSED: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 20;

unsafe extern "system" fn window_procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_CLOSE {
        // The mirror is listed in the taskbar, so the shell offers to close it, and
        // ignoring that would make the option look broken. It is not destroyed here:
        // whoever owns it has a capture and a border to wind down with it, so the
        // request is handed over and sharing stops the same way the shortcut stops it.
        // SAFETY: the value was stored by `Mirror::start` and is a window handle.
        unsafe {
            let owner = HWND(GetWindowLongPtrW(window, GWLP_USERDATA) as *mut c_void);
            if !owner.is_invalid() {
                let _ = PostMessageW(Some(owner), WM_MIRROR_CLOSED, WPARAM(0), LPARAM(0));
            }
        }
        return LRESULT(0);
    }
    // SAFETY: the arguments are passed on exactly as Windows supplied them.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

/// Permission to capture a window without Windows outlining it.
///
/// An app installed as a package has to ask for it, and the first time Windows asks the
/// user in turn. That can take as long as the user likes, so it is waited for on a
/// thread of its own while sharing starts with the outline, which is then dropped as
/// soon as the answer is yes. The package manifest declares that Stagehand may ask.
mod borderless {
    use std::sync::atomic::{AtomicU8, Ordering};

    use windows::Graphics::Capture::{GraphicsCaptureAccess, GraphicsCaptureAccessKind};
    use windows::Security::Authorization::AppCapabilityAccess::AppCapabilityAccessStatus;

    const NOT_ASKED: u8 = 0;
    const ASKED: u8 = 1;
    const ALLOWED: u8 = 2;

    static STATE: AtomicU8 = AtomicU8::new(NOT_ASKED);

    /// Asks once for as long as Stagehand runs; Windows remembers the answer after that.
    pub fn request() {
        if STATE
            .compare_exchange(NOT_ASKED, ASKED, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
        {
            return;
        }
        let asked = std::thread::Builder::new()
            .name("presenter-window-permission".to_owned())
            .spawn(|| {
                let answer = GraphicsCaptureAccess::RequestAccessAsync(
                    GraphicsCaptureAccessKind::Borderless,
                )
                .and_then(|request| request.join());
                if answer == Ok(AppCapabilityAccessStatus::Allowed) {
                    STATE.store(ALLOWED, Ordering::Relaxed);
                }
            });
        if let Err(error) = asked {
            log::warn!("could not ask to capture without an outline: {error}");
        }
    }

    pub fn is_allowed() -> bool {
        STATE.load(Ordering::Relaxed) == ALLOWED
    }
}

struct Capture {
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    device: IDirect3DDevice,
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

pub struct Mirror {
    window: HWND,
    target: HWND,
    target_rect: RECT,
    title: String,
    width: u32,
    height: u32,

    factory: ID2D1Factory1,
    device: ID3D11Device,
    device_context: ID3D11DeviceContext,
    context: ID2D1DeviceContext,
    swap_chain: IDXGISwapChain1,
    brush: ID2D1SolidColorBrush,

    // The latest picture of the window, copied into a texture of our own. The capture
    // takes its pictures back the moment they are let go of, and new ones only arrive
    // when the window changes, so without a copy the mirror would be black on every
    // frame that brought none.
    frame: Option<(ID3D11Texture2D, ID2D1Bitmap1)>,
    capture: Option<Capture>,
    // A window behind others may never repaint, and a picture is only taken when it
    // does, so until the first one arrives the window is nudged.
    borderless_applied: bool,
    has_first_frame: bool,
    last_nudge_ms: u64,
    capture_started_ms: u64,
    seed_attempted: bool,
}

impl Mirror {
    /// Starts mirroring `target`. `owner` is told with `WM_MIRROR_CLOSED` when the user
    /// closes the mirror from the taskbar.
    pub fn start(instance: HINSTANCE, target: HWND, owner: HWND, now_ms: u64) -> Result<Self> {
        let target_rect = target::visible_bounds(target).ok_or_else(windows::core::Error::empty)?;
        let width = (target_rect.right - target_rect.left) as u32;
        let height = (target_rect.bottom - target_rect.top) as u32;

        let window = create_window(instance, width, height)?;
        // SAFETY: the window was just created by this thread.
        unsafe { SetWindowLongPtrW(window, GWLP_USERDATA, owner.0 as isize) };

        let graphics = create_graphics(window, width, height);
        let (factory, device, device_context, context, swap_chain, brush) = match graphics {
            Ok(graphics) => graphics,
            Err(error) => {
                // SAFETY: the window was created above and is destroyed once.
                let _ = unsafe { DestroyWindow(window) };
                return Err(error);
            }
        };

        let mut mirror = Self {
            window,
            target,
            target_rect,
            title: target::title(target),
            width,
            height,
            factory,
            device,
            device_context,
            context,
            swap_chain,
            brush,
            frame: None,
            capture: None,
            borderless_applied: false,
            has_first_frame: false,
            last_nudge_ms: 0,
            capture_started_ms: now_ms,
            seed_attempted: false,
        };
        mirror.update_window_title();
        mirror.start_capture()?;
        // Without taking the focus: the presenter is working in another window.
        // SAFETY: the window belongs to this thread.
        let _ = unsafe { ShowWindow(window, SW_SHOWNOACTIVATE) };
        Ok(mirror)
    }

    pub fn target(&self) -> HWND {
        self.target
    }

    /// Where the mirrored window is on screen, as of the last frame.
    pub fn target_rect(&self) -> RECT {
        self.target_rect
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    /// Points the mirror at another window. The mirror window itself is kept and only
    /// resized, so a call that is already sharing it carries on. Stopping and starting
    /// again would pull the shared window out from under the viewers and make the
    /// presenter pick it a second time.
    pub fn retarget(&mut self, target: HWND, now_ms: u64) -> Result<()> {
        let bounds = target::visible_bounds(target).ok_or_else(windows::core::Error::empty)?;

        self.stop_capture();
        self.target = target;
        self.title = target::title(target);
        self.target_rect = bounds;
        self.resize_swap_chain(
            (bounds.right - bounds.left) as u32,
            (bounds.bottom - bounds.top) as u32,
        )?;
        self.update_window_title();
        self.capture_started_ms = now_ms;
        self.start_capture()
    }

    /// The title is what identifies the mirror where a window is picked for sharing, so
    /// it names the window it mirrors.
    fn update_window_title(&self) {
        let name = if self.title.is_empty() {
            "untitled window"
        } else {
            &self.title
        };
        let title = HSTRING::from(format!("Presenter Window \u{2013} {name}"));
        // SAFETY: the window belongs to this thread and `title` outlives the call.
        let _ = unsafe { SetWindowTextW(self.window, &title) };
    }

    fn start_capture(&mut self) -> Result<()> {
        // SAFETY: the device is alive, and `target` is only looked up by the system.
        let (device, item): (IDirect3DDevice, GraphicsCaptureItem) = unsafe {
            let dxgi_device: IDXGIDevice = self.device.cast()?;
            let device = CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device)?.cast()?;
            let interop =
                windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
            (device, interop.CreateForWindow(self.target)?)
        };

        // Free threaded, because pictures are taken when a frame is drawn instead of
        // being announced through a callback, which keeps everything on this thread.
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            CAPTURE_BUFFERS,
            item.Size()?,
        )?;
        let session = pool.CreateCaptureSession(&item)?;
        // Windows 11 outlines a captured window in yellow, which would follow the
        // presenter around for the whole session. Older versions cannot turn it off.
        // This is enough where no permission is needed or it was given before; see
        // `borderless` for when it has to be asked for.
        borderless::request();
        let _ = session.SetIsBorderRequired(false);
        self.borderless_applied = false;
        session.StartCapture()?;

        self.capture = Some(Capture {
            pool,
            session,
            device,
        });
        Ok(())
    }

    fn stop_capture(&mut self) {
        self.capture = None;
        self.frame = None;
        self.has_first_frame = false;
        self.last_nudge_ms = 0;
        self.seed_attempted = false;
    }

    fn resize_swap_chain(&mut self, width: u32, height: u32) -> Result<()> {
        let (width, height) = (width.max(1), height.max(1));
        if (width, height) == (self.width, self.height) {
            return Ok(());
        }
        self.width = width;
        self.height = height;
        self.frame = None;

        // SAFETY: the context lets go of the old buffer before it is resized, and the
        // window belongs to this thread.
        unsafe {
            self.context.SetTarget(None::<&ID2D1Image>);
            self.swap_chain.ResizeBuffers(
                0,
                width,
                height,
                DXGI_FORMAT_UNKNOWN,
                DXGI_SWAP_CHAIN_FLAG(0),
            )?;
            bind_back_buffer(&self.context, &self.swap_chain)?;
            // The mirror has no title bar, so its outer size is its content size.
            let _ = SetWindowPos(
                self.window,
                None,
                0,
                0,
                width as i32,
                height as i32,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        Ok(())
    }

    /// The texture the latest picture is kept in. Made again only when the size
    /// changes, so normally a frame costs one copy and nothing else.
    fn ensure_frame(&mut self) -> Option<&(ID3D11Texture2D, ID2D1Bitmap1)> {
        if self.frame.is_none() {
            let description = D3D11_TEXTURE2D_DESC {
                Width: self.width,
                Height: self.height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
                ..Default::default()
            };
            let mut texture = None;
            // SAFETY: the description and the place for the texture are live locals.
            let bitmap = unsafe {
                self.device
                    .CreateTexture2D(&description, None, Some(&mut texture))
                    .ok()?;
                let surface: IDXGISurface = texture.as_ref()?.cast().ok()?;
                self.context
                    .CreateBitmapFromDxgiSurface(
                        &surface,
                        Some(&bitmap_properties(D2D1_BITMAP_OPTIONS_NONE)),
                    )
                    .ok()?
            };
            self.frame = Some((texture?, bitmap));
        }
        self.frame.as_ref()
    }

    /// Takes the newest picture of the window, if there is one. Returns false when the
    /// capture has stopped working.
    fn take_frame(&mut self) -> bool {
        let Some(capture) = &self.capture else {
            return true;
        };
        // No picture waiting is the normal case, and reads as an error here.
        let Ok(frame) = capture.pool.TryGetNextFrame() else {
            return true;
        };

        let size = frame.ContentSize().unwrap_or_default();
        if size.Width > 0
            && size.Height > 0
            && (size.Width as u32, size.Height as u32) != (self.width, self.height)
        {
            // The window was resized: follow it, and take the next picture at the new
            // size.
            let _ = frame.Close();
            if self
                .resize_swap_chain(size.Width as u32, size.Height as u32)
                .is_err()
            {
                return false;
            }
            let Some(capture) = &self.capture else {
                return true;
            };
            return capture
                .pool
                .Recreate(
                    &capture.device,
                    DirectXPixelFormat::B8G8R8A8UIntNormalized,
                    CAPTURE_BUFFERS,
                    SizeInt32 {
                        Width: self.width as i32,
                        Height: self.height as i32,
                    },
                )
                .is_ok();
        }

        let texture = frame
            .Surface()
            .and_then(|surface| surface.cast::<IDirect3DDxgiInterfaceAccess>())
            // SAFETY: asks the picture for the texture it is kept in.
            .and_then(|access| unsafe { access.GetInterface::<ID3D11Texture2D>() });
        // Copied before the picture is let go of: it goes straight back to the pool
        // and is used again, so anything drawn from it later would be whatever landed
        // there next.
        if let Ok(texture) = texture {
            let device_context = self.device_context.clone();
            if let Some((copy, _)) = self.ensure_frame() {
                // SAFETY: both textures belong to this device and have the same size
                // and format, which the size check above makes sure of.
                unsafe { device_context.CopyResource(copy, &texture) };
                self.has_first_frame = true;
            }
        }
        let _ = frame.Close();
        true
    }

    /// Last resort for a window that never repaints: asking it to print itself draws it
    /// on demand, including windows drawn on the graphics card, which asking for a
    /// repaint cannot reach. Used once, to show something until real pictures arrive.
    fn seed_by_printing(&mut self) {
        let (width, height, target) = (self.width, self.height, self.target);
        let device_context = self.device_context.clone();
        let Some((copy, _)) = self.ensure_frame() else {
            return;
        };

        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                // A negative height makes the rows run top to bottom, as in a texture.
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        // SAFETY: every GDI object made here is released before returning, and `bits`
        // points to `width * height` pixels for as long as the bitmap is alive.
        unsafe {
            let screen = GetDC(None);
            let mut bits = std::ptr::null_mut::<c_void>();
            let bitmap = CreateDIBSection(Some(screen), &info, DIB_RGB_COLORS, &mut bits, None, 0);
            let memory = CreateCompatibleDC(Some(screen));
            ReleaseDC(None, screen);

            if let Ok(bitmap) = bitmap {
                if !memory.is_invalid() && !bits.is_null() {
                    let previous = SelectObject(memory, bitmap.into());
                    if PrintWindow(target, memory, PW_RENDERFULLCONTENT).as_bool() {
                        device_context.UpdateSubresource(copy, 0, None, bits, width * 4, 0);
                        log::info!("the presenter window showed a printed picture to begin with");
                    }
                    SelectObject(memory, previous);
                }
                let _ = DeleteObject(bitmap.into());
            }
            if !memory.is_invalid() {
                let _ = DeleteDC(memory);
            }
        }
    }

    /// Draws one frame: the window's latest picture with `shapes` on top. Returns false
    /// once the mirrored window is gone or the graphics device was lost.
    pub fn present(&mut self, shapes: &[Shape], now_ms: u64) -> bool {
        // The window going away is the normal end of a presentation.
        if !target::is_alive(self.target) {
            return false;
        }
        if let Some(rect) = target::visible_bounds(self.target) {
            self.target_rect = rect;
        }
        if !self.take_frame() {
            return false;
        }
        // The user may only now have answered whether the outline can be left out.
        if !self.borderless_applied
            && borderless::is_allowed()
            && let Some(capture) = &self.capture
        {
            let _ = capture.session.SetIsBorderRequired(false);
            self.borderless_applied = true;
        }

        // Nothing has arrived yet, which happens when the window is behind others and
        // is not repainting. Ask it to, or the mirror stays black until something else
        // makes it draw. The request is queued, so an app that does not respond cannot
        // hold this up.
        if !self.has_first_frame {
            if now_ms.saturating_sub(self.last_nudge_ms) >= NUDGE_INTERVAL_MS {
                self.last_nudge_ms = now_ms;
                // SAFETY: only queues a repaint for another window.
                let _ = unsafe {
                    RedrawWindow(
                        Some(self.target),
                        None,
                        None,
                        RDW_INVALIDATE | RDW_ALLCHILDREN,
                    )
                };
            }
            if !self.seed_attempted
                && now_ms.saturating_sub(self.capture_started_ms) >= SEED_AFTER_MS
            {
                self.seed_attempted = true;
                self.seed_by_printing();
            }
        }

        let origin = (self.target_rect.left as f32, self.target_rect.top as f32);
        let black = D2D1_COLOR_F {
            a: 1.0,
            ..Default::default()
        };
        // SAFETY: everything drawn with is owned by `self` and used on this thread
        // only; the descriptions passed by pointer are live locals.
        unsafe {
            self.context.BeginDraw();
            self.context.Clear(Some(&black));

            // Drawn on every frame from our own copy, not only when a picture arrived,
            // so a window that is not repainting still shows what it last looked like.
            if let Some((_, bitmap)) = &self.frame {
                self.context.DrawBitmap(
                    bitmap,
                    None,
                    1.0,
                    D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                    None,
                    None,
                );
            }

            // Drawing outside the mirrored window must not bleed into the mirror.
            let clip = D2D_RECT_F {
                left: 0.0,
                top: 0.0,
                right: self.width as f32,
                bottom: self.height as f32,
            };
            self.context
                .PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_ALIASED);
            for shape in shapes {
                self.fill(shape, origin);
            }
            self.context.PopAxisAlignedClip();

            self.context.EndDraw(None, None).is_ok()
                // Never waits: a window parked off screen has no display to wait for.
                && self.swap_chain.Present(0, DXGI_PRESENT(0)).is_ok()
        }
    }

    /// Fills one shape, moved from screen pixels into the mirror.
    ///
    /// # Safety
    /// Must be called between `BeginDraw` and `EndDraw` on the thread that owns `self`.
    unsafe fn fill(&self, shape: &Shape, origin: (f32, f32)) {
        let local = |&(x, y): &(f32, f32)| Vector2 {
            X: x - origin.0,
            Y: y - origin.1,
        };
        let [first, rest @ ..] = shape.outline.as_slice() else {
            return;
        };
        if rest.len() < 2 {
            return;
        }
        let [r, g, b, a] = shape.color;
        // SAFETY: the caller is drawing on this thread; the geometry and its sink are
        // made, filled and closed right here.
        unsafe {
            let Ok(geometry) = self.factory.CreatePathGeometry() else {
                return;
            };
            let Ok(sink) = geometry.Open() else { return };
            sink.SetFillMode(D2D1_FILL_MODE_WINDING);
            sink.BeginFigure(local(first), D2D1_FIGURE_BEGIN_FILLED);
            for point in rest {
                sink.AddLine(local(point));
            }
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            if sink.Close().is_err() {
                return;
            }
            self.brush.SetColor(&D2D1_COLOR_F { r, g, b, a });
            self.context
                .FillGeometry(&geometry, &self.brush, None::<&ID2D1Brush>);
        }
    }
}

impl Drop for Mirror {
    fn drop(&mut self) {
        self.stop_capture();
        // SAFETY: the context lets go of the swap chain's buffer, and the window was
        // created by this thread and is destroyed once.
        unsafe {
            self.context.SetTarget(None::<&ID2D1Image>);
            let _ = DestroyWindow(self.window);
        }
    }
}

fn create_window(instance: HINSTANCE, width: u32, height: u32) -> Result<HWND> {
    // SAFETY: the calls take no pointers and have no preconditions.
    let (left, top) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN)
                + GetSystemMetrics(SM_CXVIRTUALSCREEN)
                + OFFSCREEN_MARGIN,
            GetSystemMetrics(SM_YVIRTUALSCREEN),
        )
    };
    // Where a window is picked for sharing, the ones offered are those the shell counts
    // as applications, which is what the app window style earns. There is no title bar,
    // so that what is shared is only the mirrored window: a title bar would sit above
    // the content for every viewer.
    // SAFETY: the class is registered by `register_class` and every string is static.
    unsafe {
        CreateWindowExW(
            WS_EX_APPWINDOW,
            WINDOW_CLASS,
            w!("Presenter Window"),
            WS_POPUP,
            left,
            top,
            width as i32,
            height as i32,
            None,
            None,
            Some(instance),
            None,
        )
    }
}

type GraphicsParts = (
    ID2D1Factory1,
    ID3D11Device,
    ID3D11DeviceContext,
    ID2D1DeviceContext,
    IDXGISwapChain1,
    ID2D1SolidColorBrush,
);

fn create_graphics(window: HWND, width: u32, height: u32) -> Result<GraphicsParts> {
    // SAFETY: plain creation of graphics objects. Every pointer handed over is to a
    // live local, `window` belongs to this thread, and everything made here is used on
    // this thread only, as the single threaded Direct2D factory requires.
    unsafe {
        let factory: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let device = create_device()?;
        let device_context = device.GetImmediateContext()?;
        let dxgi_device: IDXGIDevice = device.cast()?;
        let dxgi_factory: IDXGIFactory2 = dxgi_device.GetAdapter()?.GetParent()?;

        let context = factory
            .CreateDevice(&dxgi_device)?
            .CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
        // One unit is one pixel, so the picture is never scaled.
        context.SetDpi(96.0, 96.0);

        let description = DXGI_SWAP_CHAIN_DESC1 {
            Width: width,
            Height: height,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            ..Default::default()
        };
        let swap_chain =
            dxgi_factory.CreateSwapChainForHwnd(&device, window, &description, None, None)?;
        let _ = dxgi_factory.MakeWindowAssociation(window, DXGI_MWA_NO_ALT_ENTER);
        bind_back_buffer(&context, &swap_chain)?;

        let brush = context.CreateSolidColorBrush(&D2D1_COLOR_F::default(), None)?;
        Ok((factory, device, device_context, context, swap_chain, brush))
    }
}

fn bitmap_properties(options: D2D1_BITMAP_OPTIONS) -> D2D1_BITMAP_PROPERTIES1 {
    D2D1_BITMAP_PROPERTIES1 {
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_IGNORE,
        },
        dpiX: 96.0,
        dpiY: 96.0,
        bitmapOptions: options,
        ..Default::default()
    }
}

/// Makes the context draw into the swap chain's current buffer.
///
/// # Safety
/// Both must belong to the calling thread, and the context must not be drawing.
unsafe fn bind_back_buffer(
    context: &ID2D1DeviceContext,
    swap_chain: &IDXGISwapChain1,
) -> Result<()> {
    // SAFETY: see the function's own requirements; the properties are a live local.
    unsafe {
        let buffer: IDXGISurface = swap_chain.GetBuffer(0)?;
        let properties =
            bitmap_properties(D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW);
        let bitmap = context.CreateBitmapFromDxgiSurface(&buffer, Some(&properties))?;
        context.SetTarget(&bitmap);
    }
    Ok(())
}

fn create_device() -> Result<ID3D11Device> {
    let create = |driver| {
        let mut device = None;
        // SAFETY: `device` is a live local that receives the new device.
        unsafe {
            D3D11CreateDevice(
                None,
                driver,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
        }
        .map(|()| device.expect("a successful call always supplies the device"))
    };
    match create(D3D_DRIVER_TYPE_HARDWARE) {
        // No usable graphics hardware, as in some virtual machines: draw in software.
        Err(error) if error.code() == DXGI_ERROR_UNSUPPORTED => create(D3D_DRIVER_TYPE_WARP),
        result => result,
    }
}
