//! The surface the trail is drawn on: a Direct2D context over a DirectComposition swap
//! chain. Together with a window that has no redirection bitmap, that gives per-pixel
//! transparency and hardware accelerated filling of a new path every frame.

use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED,
    D2D1_FILL_MODE_WINDING, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1,
    D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_ELLIPSE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1CreateFactory, ID2D1Brush, ID2D1DeviceContext, ID2D1Factory1, ID2D1Image,
    ID2D1SolidColorBrush, ID2D1StrokeStyle,
};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_UNKNOWN,
    DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_ERROR_UNSUPPORTED, DXGI_PRESENT, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1,
    DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT,
    IDXGIDevice, IDXGIFactory2, IDXGISurface, IDXGISwapChain1,
};
use windows::core::{Interface, Result};
use windows_numerics::Vector2;

use crate::stroke::Vec2;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgba {
    pub red: f32,
    pub green: f32,
    pub blue: f32,
    pub alpha: f32,
}

impl Rgba {
    pub fn with_alpha_scaled(self, scale: f32) -> Self {
        Self {
            alpha: self.alpha * scale,
            ..self
        }
    }

    fn native(self) -> D2D1_COLOR_F {
        D2D1_COLOR_F {
            r: self.red,
            g: self.green,
            b: self.blue,
            a: self.alpha,
        }
    }
}

/// The part of the screen the surface covers, in screen pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Bounds {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

struct Surface {
    swap_chain: IDXGISwapChain1,
    // Held so the swap chain stays attached to the window.
    _target: IDCompositionTarget,
    _visual: IDCompositionVisual,
    bounds: Bounds,
}

pub struct Graphics {
    factory: ID2D1Factory1,
    device: ID3D11Device,
    dxgi_factory: IDXGIFactory2,
    context: ID2D1DeviceContext,
    composition: IDCompositionDevice,
    brush: ID2D1SolidColorBrush,
    surface: Option<Surface>,
}

impl Graphics {
    pub fn new() -> Result<Self> {
        // SAFETY: plain factory and device creation. Every pointer handed over is to a
        // live local, and every interface is used on this thread only, which is what the
        // single threaded Direct2D factory requires.
        unsafe {
            let factory: ID2D1Factory1 =
                D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;

            let device = create_device()?;
            let dxgi_device: IDXGIDevice = device.cast()?;
            let dxgi_factory: IDXGIFactory2 = dxgi_device.GetAdapter()?.GetParent()?;

            let context = factory
                .CreateDevice(&dxgi_device)?
                .CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
            // Work in raw pixels: the surface spans every monitor, which can mix
            // scalings, so widths are scaled per monitor by the caller instead.
            context.SetDpi(96.0, 96.0);

            let composition: IDCompositionDevice = DCompositionCreateDevice(&dxgi_device)?;
            let brush = context.CreateSolidColorBrush(&D2D1_COLOR_F::default(), None)?;

            Ok(Self {
                factory,
                device,
                dxgi_factory,
                context,
                composition,
                brush,
                surface: None,
            })
        }
    }

    pub fn bounds(&self) -> Option<Bounds> {
        self.surface.as_ref().map(|surface| surface.bounds)
    }

    /// Makes the surface cover `bounds` of the screen, creating it on first use.
    pub fn resize(&mut self, window: HWND, bounds: Bounds) -> Result<()> {
        // SAFETY: `window` is the overlay window, owned by this thread and alive for as
        // long as `self`. The descriptions passed by pointer are live locals.
        unsafe {
            // The context must let go of the old buffer before it can be resized.
            self.context.SetTarget(None::<&ID2D1Image>);

            let surface = match self.surface.take() {
                Some(mut surface) => {
                    surface.swap_chain.ResizeBuffers(
                        0,
                        bounds.width,
                        bounds.height,
                        DXGI_FORMAT_UNKNOWN,
                        DXGI_SWAP_CHAIN_FLAG(0),
                    )?;
                    surface.bounds = bounds;
                    surface
                }
                None => {
                    let description = DXGI_SWAP_CHAIN_DESC1 {
                        Width: bounds.width,
                        Height: bounds.height,
                        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        SampleDesc: DXGI_SAMPLE_DESC {
                            Count: 1,
                            Quality: 0,
                        },
                        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                        BufferCount: 2,
                        Scaling: DXGI_SCALING_STRETCH,
                        SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                        AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                        ..Default::default()
                    };
                    let swap_chain = self.dxgi_factory.CreateSwapChainForComposition(
                        &self.device,
                        &description,
                        None,
                    )?;

                    let target = self.composition.CreateTargetForHwnd(window, true)?;
                    let visual = self.composition.CreateVisual()?;
                    visual.SetContent(&swap_chain)?;
                    target.SetRoot(&visual)?;
                    self.composition.Commit()?;

                    Surface {
                        swap_chain,
                        _target: target,
                        _visual: visual,
                        bounds,
                    }
                }
            };

            let buffer: IDXGISurface = surface.swap_chain.GetBuffer(0)?;
            let properties = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                ..Default::default()
            };
            let bitmap = self
                .context
                .CreateBitmapFromDxgiSurface(&buffer, Some(&properties))?;
            self.context.SetTarget(&bitmap);

            self.surface = Some(surface);
            Ok(())
        }
    }

    /// Starts a frame with nothing on it.
    pub fn begin(&self) {
        // SAFETY: the context is only used on this thread, and `end` always follows.
        unsafe {
            self.context.BeginDraw();
            self.context.Clear(Some(&D2D1_COLOR_F::default()));
        }
    }

    /// Fills a closed outline given in screen pixels.
    pub fn fill_polygon(&self, points: &[Vec2], color: Rgba) {
        let [first, rest @ ..] = points else { return };
        if rest.len() < 2 {
            return;
        }
        // SAFETY: called between `begin` and `end` on the thread that owns the context;
        // the geometry and its sink are created, filled and closed right here.
        unsafe {
            let Ok(geometry) = self.factory.CreatePathGeometry() else {
                return;
            };
            let Ok(sink) = geometry.Open() else { return };
            // The outline overlaps itself wherever the trail crosses its own path.
            // Winding fills those overlaps once, which keeps a see-through colour even.
            sink.SetFillMode(D2D1_FILL_MODE_WINDING);
            sink.BeginFigure(self.local(*first), D2D1_FIGURE_BEGIN_FILLED);
            for point in rest {
                sink.AddLine(self.local(*point));
            }
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            if sink.Close().is_err() {
                return;
            }

            self.brush.SetColor(&color.native());
            self.context
                .FillGeometry(&geometry, &self.brush, None::<&ID2D1Brush>);
        }
    }

    /// Draws a circle outline around a centre given in screen pixels.
    pub fn draw_ring(&self, centre: Vec2, radius: f32, thickness: f32, color: Rgba) {
        let ring = D2D1_ELLIPSE {
            point: self.local(centre),
            radiusX: radius,
            radiusY: radius,
        };
        // SAFETY: called between `begin` and `end` on the thread that owns the context;
        // `ring` is a live local.
        unsafe {
            self.brush.SetColor(&color.native());
            self.context
                .DrawEllipse(&ring, &self.brush, thickness, None::<&ID2D1StrokeStyle>);
        }
    }

    /// Puts the frame on screen. Returns false when the graphics device went away, in
    /// which case everything here has to be created again.
    #[must_use]
    pub fn end(&self) -> bool {
        // SAFETY: balances `begin` on the same thread; presenting takes no pointers.
        unsafe {
            if self.context.EndDraw(None, None).is_err() {
                return false;
            }
            // Never wait for the display: the mouse hook runs on this thread, and
            // blocking here would make the whole system's pointer stutter.
            self.surface
                .as_ref()
                .is_some_and(|surface| surface.swap_chain.Present(0, DXGI_PRESENT(0)).is_ok())
        }
    }

    /// Shifts a point from screen pixels into the surface.
    fn local(&self, point: Vec2) -> Vector2 {
        let bounds = self.bounds().unwrap_or_default();
        Vector2 {
            X: point.x - bounds.left as f32,
            Y: point.y - bounds.top as f32,
        }
    }
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
