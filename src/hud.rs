//! Crops a HUD region, cleans it up for OCR and reads its lines with ocrs (pure Rust OCR).
use crate::config::{OcrCfg, Region};
use anyhow::{Context, Result};
use image::{imageops, GrayImage, Luma, RgbaImage};
use ocrs::{ImageSource, OcrEngine, OcrEngineParams, TextItem};
use rten::Model;

pub struct Reader {
    engine: OcrEngine,
    cfg: OcrCfg,
}

impl Reader {
    pub fn new(cfg: &OcrCfg) -> Result<Self> {
        let det = Model::load_file(&cfg.detection_model)
            .with_context(|| format!("loading {} (see README: download the models)", cfg.detection_model))?;
        let rec = Model::load_file(&cfg.recognition_model)
            .with_context(|| format!("loading {}", cfg.recognition_model))?;
        let engine = OcrEngine::new(OcrEngineParams {
            detection_model: Some(det),
            recognition_model: Some(rec),
            ..Default::default()
        })?;
        Ok(Self { engine, cfg: cfg.clone() })
    }

    /// Crop + greyscale + upscale + (optional) invert / threshold.
    pub fn prepare(&self, frame: &RgbaImage, region: &Region) -> GrayImage {
        prepare(frame, region, &self.cfg)
    }

    /// OCR one region; returns its text lines, top to bottom.
    pub fn read_lines(&self, frame: &RgbaImage, region: &Region) -> Result<Vec<String>> {
        Ok(self.read_positioned(frame, region)?.into_iter().map(|(t, _)| t).collect())
    }

    /// Like `read_lines`, with the right edge of every character as a fraction of the region width
    /// (to tell the balance box at the HUD's right margin from the match-change box left of it).
    pub fn read_positioned(&self, frame: &RgbaImage, region: &Region) -> Result<Vec<(String, Vec<f32>)>> {
        let img = self.prepare(frame, region);
        let rgb = image::DynamicImage::ImageLuma8(img).into_rgb8();
        let src = ImageSource::from_bytes(rgb.as_raw(), rgb.dimensions())?;
        let input = self.engine.prepare_input(src)?;
        let words = self.engine.detect_words(&input)?;
        let lines = self.engine.find_text_lines(&input, &words);
        let texts = self.engine.recognize_text(&input, &lines)?;
        let w = rgb.width().max(1) as f32;
        Ok(texts
            .into_iter()
            .flatten()
            .map(|l| {
                let xs = l.chars().iter().map(|c| c.rect.right() as f32 / w).collect();
                (l.to_string(), xs)
            })
            .filter(|(s, _)| s.trim().len() > 1)
            .collect())
    }
}

pub fn prepare(frame: &RgbaImage, region: &Region, cfg: &OcrCfg) -> GrayImage {
    let (x, y, w, h) = region.rect(frame.width(), frame.height());
    let crop = imageops::crop_imm(frame, x, y, w, h).to_image();
    let mut g: GrayImage = image::DynamicImage::ImageRgba8(crop).into_luma8();
    if cfg.scale > 1.01 {
        let (nw, nh) = ((w as f32 * cfg.scale) as u32, (h as f32 * cfg.scale) as u32);
        g = imageops::resize(&g, nw.max(1), nh.max(1), imageops::FilterType::CatmullRom);
    }
    for p in g.pixels_mut() {
        let mut v = p.0[0];
        if cfg.threshold > 0 {
            v = if v >= cfg.threshold { 255 } else { 0 };
        }
        if cfg.invert {
            v = 255 - v;
        }
        *p = Luma([v]);
    }
    g
}
