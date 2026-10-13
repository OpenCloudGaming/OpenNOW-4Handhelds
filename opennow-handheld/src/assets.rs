//! Icons and brand artwork shared with the Qt shell, rasterised on demand at
//! the exact pixel size they are drawn at. Output is premultiplied RGBA.

use resvg::tiny_skia::{FilterQuality, IntSize, Pixmap, PixmapPaint, Transform};
use resvg::usvg;

#[derive(Clone, Copy, Hash, PartialEq, Eq)]
pub enum Image {
    Logo,
    Store,
    Settings,
    Power,
}

fn svg_source(image: Image) -> Option<&'static [u8]> {
    match image {
        Image::Store => Some(include_bytes!("../assets/shelf-store.svg")),
        Image::Settings => Some(include_bytes!("../assets/nav-settings.svg")),
        Image::Power => Some(include_bytes!("../assets/shelf-power.svg")),
        Image::Logo => None,
    }
}

const LOGO: &[u8] = include_bytes!("../assets/opennow-mark.png");

pub fn rasterize(image: Image, width: u32, height: u32) -> Vec<u8> {
    let Some(mut target) = Pixmap::new(width, height) else {
        return vec![0; (width * height * 4) as usize];
    };
    match svg_source(image) {
        Some(source) => {
            if let Ok(tree) = usvg::Tree::from_data(source, &usvg::Options::default()) {
                let size = tree.size();
                let transform = Transform::from_scale(width as f32 / size.width(), height as f32 / size.height());
                resvg::render(&tree, transform, &mut target.as_mut());
            }
        }
        None => {
            if let Some(logo) = decode_png(LOGO) {
                let transform = Transform::from_scale(
                    width as f32 / logo.width() as f32,
                    height as f32 / logo.height() as f32,
                );
                let paint = PixmapPaint { quality: FilterQuality::Bicubic, ..PixmapPaint::default() };
                target.draw_pixmap(0, 0, logo.as_ref(), &paint, transform, None);
            }
        }
    }
    target.take()
}

/// Opens the brand artwork as a premultiplied pixmap.
fn decode_png(bytes: &[u8]) -> Option<Pixmap> {
    let mut decoder = png::Decoder::new(bytes);
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let mut buffer = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buffer).ok()?;
    let pixels = &buffer[..info.buffer_size()];
    let mut rgba = Vec::with_capacity((info.width * info.height * 4) as usize);
    match info.color_type {
        png::ColorType::Rgba => {
            for pixel in pixels.chunks_exact(4) {
                let alpha = pixel[3] as u16;
                for channel in &pixel[..3] {
                    rgba.push(((*channel as u16 * alpha + 127) / 255) as u8);
                }
                rgba.push(pixel[3]);
            }
        }
        png::ColorType::Rgb => {
            for pixel in pixels.chunks_exact(3) {
                rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
            }
        }
        png::ColorType::GrayscaleAlpha => {
            for pixel in pixels.chunks_exact(2) {
                let value = ((pixel[0] as u16 * pixel[1] as u16 + 127) / 255) as u8;
                rgba.extend_from_slice(&[value, value, value, pixel[1]]);
            }
        }
        png::ColorType::Grayscale => {
            for value in pixels {
                rgba.extend_from_slice(&[*value, *value, *value, 255]);
            }
        }
        png::ColorType::Indexed => return None,
    }
    Pixmap::from_vec(rgba, IntSize::from_wh(info.width, info.height)?)
}
