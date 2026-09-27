//! Where frames come from: the game window / a monitor (Windows), or a folder of screenshots (anywhere,
//! for testing without the game).
use anyhow::{bail, Result};
use image::RgbaImage;
use std::path::{Path, PathBuf};

pub trait FrameSource {
    /// Next frame, or None when the source is finished (end of a replay folder).
    fn next_frame(&mut self) -> Result<Option<RgbaImage>>;
}

/// Screenshots in a folder, in name order. Each file is one frame.
pub struct FolderReplay {
    files: Vec<PathBuf>,
    i: usize,
}

impl FolderReplay {
    pub fn new(dir: &Path) -> Result<Self> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                matches!(
                    p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
                    Some("png" | "jpg" | "jpeg")
                )
            })
            .collect();
        files.sort();
        if files.is_empty() {
            bail!("no .png/.jpg in {}", dir.display());
        }
        Ok(Self { files, i: 0 })
    }
}

impl FrameSource for FolderReplay {
    fn next_frame(&mut self) -> Result<Option<RgbaImage>> {
        let Some(p) = self.files.get(self.i) else { return Ok(None) };
        self.i += 1;
        eprintln!("[replay] {}", p.display());
        Ok(Some(image::open(p)?.into_rgba8()))
    }
}

#[cfg(windows)]
pub use live::LiveCapture;

#[cfg(windows)]
mod live {
    use super::*;
    use crate::config::CaptureCfg;
    use xcap::{Monitor, Window};

    /// Captures the game window by title (preferred: works when another window covers it), or a monitor.
    /// Only reads pixels through the Windows capture APIs, like OBS: it never touches the game process.
    pub struct LiveCapture {
        cfg: CaptureCfg,
        window: Option<Window>,
    }

    impl LiveCapture {
        pub fn new(cfg: &CaptureCfg) -> Result<Self> {
            Ok(Self { cfg: cfg.clone(), window: None })
        }

        fn find_window(&self) -> Option<Window> {
            let want = self.cfg.window_title.to_lowercase();
            Window::all().ok()?.into_iter().find(|w| {
                w.title().map(|t| t.to_lowercase().contains(&want)).unwrap_or(false)
                    && !w.is_minimized().unwrap_or(true)
            })
        }
    }

    impl FrameSource for LiveCapture {
        fn next_frame(&mut self) -> Result<Option<RgbaImage>> {
            if self.cfg.window_title.is_empty() {
                let monitors = Monitor::all()?;
                let Some(m) = monitors.get(self.cfg.monitor) else {
                    bail!("monitor {} not found ({} monitors)", self.cfg.monitor, monitors.len())
                };
                return Ok(Some(m.capture_image()?));
            }
            if self.window.is_none() {
                self.window = self.find_window();
            }
            let Some(w) = &self.window else {
                // game not running yet: an empty frame, the loop just waits
                return Ok(Some(RgbaImage::new(1, 1)));
            };
            match w.capture_image() {
                Ok(img) => Ok(Some(img)),
                Err(_) => {
                    self.window = None; // window closed or recreated: look again next time
                    Ok(Some(RgbaImage::new(1, 1)))
                }
            }
        }
    }
}
