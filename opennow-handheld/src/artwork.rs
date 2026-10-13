//! Box-art decoding off the UI thread. Cached files from the core's artwork
//! cache are decoded, centre-cropped to a square, and area-downscaled to one
//! fixed texture size, producing premultiplied RGBA ready for upload.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};

/// Square side of every artwork texture. Tiles are 172 design units, which is
/// 1:1 on the R36S and leaves headroom for a scaled desktop window.
pub const SIZE: u32 = 256;
/// Encoded files above this are skipped rather than decoded on a 1 GB device.
const MAXIMUM_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAXIMUM_PIXELS: usize = 4096 * 4096;

pub struct Decoded {
    pub key: String,
    pub pixels: Option<Vec<u8>>,
}

pub struct Decoder {
    jobs: Sender<(String, PathBuf)>,
    results: Receiver<Decoded>,
}

impl Decoder {
    pub fn new() -> Self {
        let (jobs, job_receiver) = channel::<(String, PathBuf)>();
        let (result_sender, results) = channel();
        std::thread::Builder::new()
            .name("opennow-artwork".into())
            .spawn(move || {
                for (key, path) in job_receiver {
                    let pixels = decode_file(&path);
                    if pixels.is_none() {
                        eprintln!("artwork: could not decode {}", path.display());
                    }
                    if result_sender.send(Decoded { key, pixels }).is_err() {
                        break;
                    }
                }
            })
            .expect("artwork thread starts");
        Self { jobs, results }
    }

    pub fn submit(&self, key: String, path: PathBuf) {
        let _ = self.jobs.send((key, path));
    }

    pub fn finished(&self) -> Vec<Decoded> {
        self.results.try_iter().collect()
    }
}

fn decode_file(path: &PathBuf) -> Option<Vec<u8>> {
    if std::fs::metadata(path).ok()?.len() > MAXIMUM_FILE_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let (width, height, rgb) = if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        decode_jpeg(&bytes)?
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        decode_png(&bytes)?
    } else {
        return None;
    };
    Some(cover(&rgb, width, height, SIZE))
}

fn decode_jpeg(bytes: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
    let mut decoder = jpeg_decoder::Decoder::new(bytes);
    decoder.read_info().ok()?;
    let info = decoder.info()?;
    let (width, height) = (info.width as usize, info.height as usize);
    if width == 0 || height == 0 || width * height > MAXIMUM_PIXELS {
        return None;
    }
    let pixels = decoder.decode().ok()?;
    let rgb = match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => pixels,
        jpeg_decoder::PixelFormat::L8 => pixels.iter().flat_map(|&value| [value, value, value]).collect(),
        jpeg_decoder::PixelFormat::CMYK32 => pixels
            .chunks_exact(4)
            .flat_map(|pixel| {
                let k = pixel[3] as u16;
                [pixel[0], pixel[1], pixel[2]].map(|channel| ((channel as u16 * k) / 255) as u8)
            })
            .collect(),
        jpeg_decoder::PixelFormat::L16 => pixels.chunks_exact(2).flat_map(|value| [value[0]; 3]).collect(),
    };
    Some((width, height, rgb))
}

fn decode_png(bytes: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
    let mut decoder = png::Decoder::new(bytes);
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let (width, height) = (reader.info().width as usize, reader.info().height as usize);
    if width == 0 || height == 0 || width * height > MAXIMUM_PIXELS {
        return None;
    }
    let mut buffer = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buffer).ok()?;
    let pixels = &buffer[..info.buffer_size()];
    let rgb = match info.color_type {
        png::ColorType::Rgb => pixels.to_vec(),
        png::ColorType::Rgba => pixels.chunks_exact(4).flat_map(|pixel| [pixel[0], pixel[1], pixel[2]]).collect(),
        png::ColorType::Grayscale => pixels.iter().flat_map(|&value| [value; 3]).collect(),
        png::ColorType::GrayscaleAlpha => pixels.chunks_exact(2).flat_map(|pixel| [pixel[0]; 3]).collect(),
        png::ColorType::Indexed => return None,
    };
    Some((width, height, rgb))
}

/// Crops the centre square and averages each source area into one target
/// pixel, which avoids the aliasing of point or bilinear downscaling.
fn cover(rgb: &[u8], width: usize, height: usize, size: u32) -> Vec<u8> {
    let size = size as usize;
    let side = width.min(height) as f32;
    let (left, top) = ((width as f32 - side) / 2.0, (height as f32 - side) / 2.0);
    let step = side / size as f32;
    let mut output = Vec::with_capacity(size * size * 4);
    for row in 0..size {
        let y0 = (top + row as f32 * step) as usize;
        let y1 = ((top + (row + 1) as f32 * step) as usize).clamp(y0 + 1, height);
        for column in 0..size {
            let x0 = (left + column as f32 * step) as usize;
            let x1 = ((left + (column + 1) as f32 * step) as usize).clamp(x0 + 1, width);
            let mut sum = [0u32; 3];
            for y in y0..y1 {
                let line = &rgb[(y * width + x0) * 3..(y * width + x1) * 3];
                for pixel in line.chunks_exact(3) {
                    sum[0] += pixel[0] as u32;
                    sum[1] += pixel[1] as u32;
                    sum[2] += pixel[2] as u32;
                }
            }
            let count = ((y1 - y0) * (x1 - x0)) as u32;
            output.extend_from_slice(&[(sum[0] / count) as u8, (sum[1] / count) as u8, (sum[2] / count) as u8, 255]);
        }
    }
    output
}
