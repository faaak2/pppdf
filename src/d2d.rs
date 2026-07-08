//! Direct2D/DirectWrite: device management and ALL drawing.

use crate::app::{App, Mode};
use crate::layout::{page_rect, PointF, RectF, SizeU};
use windows::core::*;
use windows::Win32::Foundation::{D2DERR_RECREATE_TARGET, HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

const BG: D2D1_COLOR_F = D2D1_COLOR_F { r: 0.102, g: 0.102, b: 0.102, a: 1.0 }; // #1a1a1a

fn color(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
}

fn rectf(r: RectF) -> D2D_RECT_F {
    D2D_RECT_F { left: r.left, top: r.top, right: r.right, bottom: r.bottom }
}

pub struct Renderer {
    factory: ID2D1Factory,
    dwrite: IDWriteFactory,
    rt: Option<ID2D1HwndRenderTarget>,
    /// Memo of the last uploaded page bitmap: (page, px) -> device bitmap.
    page_bitmap: Option<(u16, SizeU, ID2D1Bitmap)>,
    dpi: u32,
    /// ((size_dip*10) as u32, centered) -> format; rebuilt when DPI changes.
    formats: Vec<((u32, bool), IDWriteTextFormat)>,
}

impl Renderer {
    pub fn new() -> Result<Renderer> {
        unsafe {
            let factory: ID2D1Factory =
                D2D1CreateFactory::<ID2D1Factory>(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            Ok(Renderer {
                factory,
                dwrite,
                rt: None,
                page_bitmap: None,
                dpi: 96,
                formats: Vec::new(),
            })
        }
    }

    pub fn set_dpi(&mut self, dpi: u32) {
        self.dpi = dpi;
        self.formats.clear();
    }

    /// Forget the memoized device bitmap. MUST be called when a new document
    /// loads — a same-sized page 0 of the new document would otherwise show
    /// the old document's pixels.
    pub fn clear_page_cache(&mut self) {
        self.page_bitmap = None;
    }

    fn scale(&self) -> f32 {
        self.dpi as f32 / 96.0
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        if let Some(rt) = &self.rt {
            unsafe {
                let _ = rt.Resize(&D2D_SIZE_U { width: w.max(1), height: h.max(1) });
            }
        }
    }

    fn ensure_rt(&mut self, hwnd: HWND) -> Result<ID2D1HwndRenderTarget> {
        if let Some(rt) = &self.rt {
            return Ok(rt.clone());
        }
        unsafe {
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_IGNORE,
                },
                // 96 dpi = 1 DIP == 1 physical px; we do our own DPI scaling.
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd,
                pixelSize: D2D_SIZE_U {
                    width: (rc.right - rc.left).max(1) as u32,
                    height: (rc.bottom - rc.top).max(1) as u32,
                },
                presentOptions: D2D1_PRESENT_OPTIONS_NONE,
            };
            let rt = self.factory.CreateHwndRenderTarget(&props, &hwnd_props)?;
            self.rt = Some(rt.clone());
            Ok(rt)
        }
    }

    fn text_format(&mut self, size_dip: f32, centered: bool) -> Result<IDWriteTextFormat> {
        let key = ((size_dip * 10.0) as u32, centered);
        if let Some((_, f)) = self.formats.iter().find(|(k, _)| *k == key) {
            return Ok(f.clone());
        }
        unsafe {
            let make = |family: PCWSTR| {
                self.dwrite.CreateTextFormat(
                    family,
                    None,
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size_dip * self.scale(),
                    w!("en-us"),
                )
            };
            // Spec: Segoe UI Variable, with plain Segoe UI as the fallback.
            let f = make(w!("Segoe UI Variable")).or_else(|_| make(w!("Segoe UI")))?;
            if centered {
                f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
                f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            }
            self.formats.push((key, f.clone()));
            Ok(f)
        }
    }

    fn draw_text(
        &self,
        rt: &ID2D1HwndRenderTarget,
        text: &str,
        fmt: &IDWriteTextFormat,
        rect: RectF,
        col: D2D1_COLOR_F,
    ) -> Result<()> {
        unsafe {
            let brush = rt.CreateSolidColorBrush(&col, None)?;
            let utf16: Vec<u16> = text.encode_utf16().collect();
            rt.DrawText(
                &utf16,
                fmt,
                &rectf(rect),
                &brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
        Ok(())
    }

    fn page_bitmap(
        &mut self,
        rt: &ID2D1HwndRenderTarget,
        page: u16,
        px: SizeU,
        bgra: &[u8],
    ) -> Result<ID2D1Bitmap> {
        if let Some((p, s, bmp)) = &self.page_bitmap {
            if *p == page && *s == px {
                return Ok(bmp.clone());
            }
        }
        unsafe {
            let props = D2D1_BITMAP_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_IGNORE,
                },
                dpiX: 96.0,
                dpiY: 96.0,
            };
            let bmp = rt.CreateBitmap(
                D2D_SIZE_U { width: px.w, height: px.h },
                Some(bgra.as_ptr() as *const core::ffi::c_void),
                px.w * 4,
                &props,
            )?;
            self.page_bitmap = Some((page, px, bmp.clone()));
            Ok(bmp)
        }
    }

    /// Draw the whole scene. On device loss, drops device resources and
    /// returns Ok — the next WM_PAINT recreates everything.
    pub fn draw(&mut self, hwnd: HWND, app: &App, badge: Option<(&str, f32)>) -> Result<()> {
        let rt = self.ensure_rt(hwnd)?;
        let result = self.draw_inner(&rt, app, badge);
        match result {
            Err(e) if e.code() == D2DERR_RECREATE_TARGET => {
                self.rt = None;
                self.page_bitmap = None;
                Ok(())
            }
            other => other,
        }
    }

    fn draw_inner(
        &mut self,
        rt: &ID2D1HwndRenderTarget,
        app: &App,
        badge: Option<(&str, f32)>,
    ) -> Result<()> {
        unsafe {
            rt.BeginDraw();
            rt.Clear(Some(&BG));
            let client = app.client;
            let full = RectF { left: 0.0, top: 0.0, right: client.w as f32, bottom: client.h as f32 };

            match &app.mode {
                Mode::Empty => {
                    let fmt = self.text_format(16.0, true)?;
                    self.draw_text(rt, "Right-click or drop a PDF", &fmt, full, color(1.0, 1.0, 1.0, 0.35))?;
                }
                Mode::Loading { .. } => {
                    let fmt = self.text_format(16.0, true)?;
                    self.draw_text(rt, "Loading…", &fmt, full, color(1.0, 1.0, 1.0, 0.35))?;
                }
                Mode::Password { buf, .. } => {
                    let fmt = self.text_format(16.0, true)?;
                    let dots = "•".repeat(buf.chars().count().max(0));
                    let msg =
                        format!("Password required\n\n{dots}_\n\nEnter to open — Esc to cancel");
                    self.draw_text(rt, &msg, &fmt, full, color(1.0, 1.0, 1.0, 0.8))?;
                }
                Mode::Doc(d) => {
                    if let (Some(px), Some(dest)) = (app.current_px(), app.current_page_rect()) {
                        if let Some(entry) = app.cache.get_best(d.current, px) {
                            let bmp = self.page_bitmap(rt, entry.page, entry.px, &entry.bgra)?;
                            let crisp = entry.px.w as f32 == dest.width().round()
                                && entry.px.h as f32 == dest.height().round();
                            let mode = if crisp {
                                D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR
                            } else {
                                D2D1_BITMAP_INTERPOLATION_MODE_LINEAR
                            };
                            rt.DrawBitmap(&bmp, Some(&rectf(dest)), 1.0, mode, None);
                        }
                    }
                    // Overlay + badge drawing lands in Task 8.
                    let _ = badge;
                }
            }
            rt.EndDraw(None, None)
        }
    }
}
