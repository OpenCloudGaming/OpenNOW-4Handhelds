//! Nunito, the shell's typeface, rasterised with swash. The variable font
//! covers every weight the screens use from one embedded file.

use swash::scale::{Render, ScaleContext, Source, image::Content};
use swash::zeno::Format;
use swash::{FontRef, NormalizedCoord};
use std::collections::HashMap;

const NUNITO: &[u8] = include_bytes!("../assets/Nunito-Variable.ttf");

#[derive(Clone, Copy, Hash, PartialEq, Eq)]
pub enum Weight {
    SemiBold,
    Bold,
    ExtraBold,
    Black,
}

impl Weight {
    fn value(self) -> f32 {
        match self {
            Weight::SemiBold => 600.0,
            Weight::Bold => 700.0,
            Weight::ExtraBold => 800.0,
            Weight::Black => 900.0,
        }
    }
}

pub struct RenderedGlyph {
    pub width: u32,
    pub height: u32,
    pub left: i32,
    pub top: i32,
    pub advance: f32,
    pub coverage: Vec<u8>,
}

pub struct Fonts {
    context: ScaleContext,
    coords: HashMap<Weight, Vec<NormalizedCoord>>,
}

fn font() -> FontRef<'static> {
    FontRef::from_index(NUNITO, 0).expect("embedded Nunito font is valid")
}

impl Fonts {
    pub fn new() -> Self {
        Self { context: ScaleContext::new(), coords: HashMap::new() }
    }

    fn coords(&mut self, weight: Weight) -> Vec<NormalizedCoord> {
        self.coords
            .entry(weight)
            .or_insert_with(|| font().variations().normalized_coords([("wght", weight.value())]).collect())
            .clone()
    }

    pub fn glyph(&self, character: char) -> u16 {
        font().charmap().map(character)
    }

    pub fn ascent(&mut self, pixels: f32, weight: Weight) -> f32 {
        let coords = self.coords(weight);
        font().metrics(&coords).scale(pixels).ascent
    }

    pub fn render(&mut self, glyph: u16, pixels: f32, weight: Weight) -> RenderedGlyph {
        let coords = self.coords(weight);
        let font = font();
        let advance = font.glyph_metrics(&coords).scale(pixels).advance_width(glyph);
        let mut scaler = self
            .context
            .builder(font)
            .size(pixels)
            .hint(false)
            .normalized_coords(coords.iter())
            .build();
        let image = Render::new(&[Source::Outline]).format(Format::Alpha).render(&mut scaler, glyph);
        match image {
            Some(image) if image.content == Content::Mask => RenderedGlyph {
                width: image.placement.width,
                height: image.placement.height,
                left: image.placement.left,
                top: image.placement.top,
                advance,
                coverage: image.data,
            },
            _ => RenderedGlyph { width: 0, height: 0, left: 0, top: 0, advance, coverage: Vec::new() },
        }
    }
}
