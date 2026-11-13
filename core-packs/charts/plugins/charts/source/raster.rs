//! SVG to PNG with the same embedded font, so the picture and the vector
//! always agree. The PNG is opaque RGB, encoded with fixed settings.

use std::sync::Arc;

use resvg::tiny_skia::{Pixmap, Transform};
use resvg::usvg::{self, fontdb};

use crate::err::{Res, fail};
use crate::text::{BOLD, FAMILY, REGULAR};

/// A rendered image.
pub struct Png {
    /// PNG file bytes.
    pub bytes: Vec<u8>,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

fn options() -> usvg::Options<'static> {
    let mut db = fontdb::Database::new();
    db.load_font_data(REGULAR.to_vec());
    db.load_font_data(BOLD.to_vec());
    db.set_sans_serif_family(FAMILY);
    db.set_serif_family(FAMILY);
    db.set_monospace_family(FAMILY);
    usvg::Options {
        fontdb: Arc::new(db),
        font_family: FAMILY.to_owned(),
        ..usvg::Options::default()
    }
}

/// Renders `svg` at `scale` times its own size.
pub fn render(svg: &str, scale: f64) -> Res<Png> {
    let tree = usvg::Tree::from_str(svg, &options())
        .map_err(|e| format!("the chart picture could not be drawn: {e}"))?;
    let size = tree.size();
    let width = (f64::from(size.width()) * scale).round() as u32;
    let height = (f64::from(size.height()) * scale).round() as u32;
    let Some(mut pixmap) = Pixmap::new(width.max(1), height.max(1)) else {
        return fail("the image is too large to draw; lower width, height or scale");
    };
    let sx = width as f32 / size.width();
    let sy = height as f32 / size.height();
    resvg::render(&tree, Transform::from_scale(sx, sy), &mut pixmap.as_mut());
    // The background is opaque, so the premultiplied pixels are the colours.
    let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
    for px in pixmap.data().as_chunks::<4>().0 {
        rgb.extend_from_slice(&px[..3]);
    }
    let mut bytes = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut bytes, width, height);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Balanced);
        let mut w = enc
            .write_header()
            .map_err(|e| format!("the PNG could not be written: {e}"))?;
        w.write_image_data(&rgb)
            .map_err(|e| format!("the PNG could not be written: {e}"))?;
    }
    Ok(Png {
        bytes,
        width,
        height,
    })
}

/// Decodes PNG bytes to (width, height, RGB pixels); used by tests and
/// checks of the output.
pub fn decode(bytes: &[u8]) -> Res<(u32, u32, Vec<u8>)> {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info().map_err(|e| format!("not a PNG: {e}"))?;
    let mut buf = vec![
        0;
        reader
            .output_buffer_size()
            .ok_or("not a PNG: it is too large")?
    ];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("not a PNG: {e}"))?;
    buf.truncate(info.buffer_size());
    Ok((info.width, info.height, buf))
}
