//! Windows color emoji for the Stats achievements, as Slint images. The software
//! renderer draws only monochrome glyphs, so each emoji is drawn once through
//! DirectWrite with color glyphs into a transparent WIC bitmap — the same Segoe UI
//! Emoji glyphs v2 showed — and handed to Slint as premultiplied RGBA.

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};
use windows::{
    core::{w, Result},
    Win32::{
        Graphics::{
            Direct2D::{
                Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F},
                D2D1CreateFactory, ID2D1Factory, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
                D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_RENDER_TARGET_PROPERTIES,
                D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
            },
            DirectWrite::{
                DWriteCreateFactory, IDWriteFactory, DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL,
                DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_MEASURING_MODE_NATURAL,
                DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER,
            },
            Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
            Imaging::{
                CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory,
                WICBitmapCacheOnLoad, WICBitmapLockRead, WICRect,
            },
        },
        System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED},
    },
};

/// `emoji` drawn in a `px`×`px` square; `gray` = desaturated like CSS `grayscale(1)`.
/// None if any Windows imaging call fails (the caller shows no icon).
pub fn render(emoji: &str, px: u32, gray: bool) -> Option<Image> {
    let mut buf = draw(emoji, px).map_err(|err| eprintln!("emoji {emoji}: {err}")).ok()?;
    if gray {
        for p in buf.make_mut_slice() {
            // Rec. 709 luma on premultiplied channels stays premultiplied.
            let y = (0.2126 * p.r as f32 + 0.7152 * p.g as f32 + 0.0722 * p.b as f32).round() as u8;
            (p.r, p.g, p.b) = (y, y, y);
        }
    }
    Some(Image::from_rgba8_premultiplied(buf))
}

fn draw(emoji: &str, px: u32) -> Result<SharedPixelBuffer<Rgba8Pixel>> {
    unsafe {
        // The event-loop thread already has COM (winit); this is a no-op there.
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let bitmap = wic.CreateBitmap(px, px, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnLoad)?;
        let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            ..Default::default()
        };
        let rt = d2d.CreateWicBitmapRenderTarget(&bitmap, &props)?;
        rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE); // no ClearType on transparency

        let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
        let format = dw.CreateTextFormat(
            w!("Segoe UI Emoji"),
            None,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            px as f32 * 0.78,
            w!("en-us"),
        )?;
        format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
        format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        let brush = rt.CreateSolidColorBrush(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 1.0 }, None)?;

        let text: Vec<u16> = emoji.encode_utf16().collect();
        let side = px as f32;
        rt.BeginDraw();
        rt.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
        rt.DrawText(
            &text,
            &format,
            &D2D_RECT_F { left: 0.0, top: 0.0, right: side, bottom: side },
            &brush,
            D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
            DWRITE_MEASURING_MODE_NATURAL,
        );
        rt.EndDraw(None, None)?;

        let lock = bitmap.Lock(&WICRect { X: 0, Y: 0, Width: px as i32, Height: px as i32 }, WICBitmapLockRead.0 as u32)?;
        let stride = lock.GetStride()? as usize;
        let (mut size, mut data) = (0u32, std::ptr::null_mut());
        lock.GetDataPointer(&mut size, &mut data)?;
        let src = std::slice::from_raw_parts(data, size as usize);
        let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(px, px);
        for (y, row) in buf.make_mut_slice().chunks_mut(px as usize).enumerate() {
            for (x, p) in row.iter_mut().enumerate() {
                let i = y * stride + x * 4; // BGRA
                *p = Rgba8Pixel { r: src[i + 2], g: src[i + 1], b: src[i], a: src[i + 3] };
            }
        }
        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixels(emoji: &str, gray: bool) -> Vec<Rgba8Pixel> {
        let mut buf = draw(emoji, 48).expect("draw failed");
        if gray {
            let img = render(emoji, 48, true).unwrap();
            buf = img.to_rgba8_premultiplied().unwrap();
        }
        buf.as_slice().to_vec()
    }

    #[test]
    fn emoji_render_in_color_and_gray() {
        let color = pixels("🎯", false);
        let opaque = color.iter().filter(|p| p.a > 200).count();
        assert!(opaque > 300, "too few opaque pixels: {opaque}");
        assert!(color.iter().any(|p| p.a > 200 && (p.r as i32 - p.b as i32).abs() > 60), "no color glyph");
        let gray = pixels("🎯", true);
        assert!(gray.iter().all(|p| p.r == p.g && p.g == p.b));
    }
}
