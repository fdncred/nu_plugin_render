//! Draws nushell tables as pixels and shows them with the kitty graphics
//! protocol.
//!
//! The way from a value to pixels touches no terminal: [`model`] turns a
//! value into a grid of styled text, [`layout`] measures it with real
//! fonts, and [`paint`] draws it into RGBA bands. A string instead goes
//! through [`text`], which keeps it on a grid of terminal cells in its own
//! ANSI colors. Only `term` talks to the terminal, and [`kitty`] writes the
//! escape codes.

pub mod ansi;
pub mod border;
mod command;
pub mod draw;
pub mod fonts;
pub mod kitty;
pub mod layout;
pub mod model;
pub mod paint;
pub mod sixel;
pub mod style;
mod term;
pub mod text;

use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use nu_plugin::{Plugin, PluginCommand};

/// The plugin. The system font list takes a while to read, so that happens
/// once per plugin process, on the first call.
#[derive(Default)]
pub struct RenderPlugin {
    fonts: OnceLock<Result<Arc<fonts::Fonts>, String>>,
    /// The clipboard, kept open between calls. On Linux, copied data lasts
    /// only as long as the process that copied it holds the clipboard,
    /// unless a clipboard manager takes it over.
    clipboard: Mutex<Option<arboard::Clipboard>>,
    /// The terminal's answers to the first probe. Later calls reuse them
    /// and only read the window size, so they never read from the
    /// terminal, where keys typed meanwhile would be lost.
    caps: Mutex<Option<term::Caps>>,
}

impl RenderPlugin {
    fn fonts(&self) -> Result<Arc<fonts::Fonts>, String> {
        self.fonts
            .get_or_init(|| fonts::Fonts::load().map(Arc::new))
            .clone()
    }

    /// Put an RGBA image on the clipboard.
    fn copy_image(&self, width: u32, height: u32, rgba: Vec<u8>) -> Result<(), String> {
        let mut clipboard = self
            .clipboard
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if clipboard.is_none() {
            *clipboard = Some(arboard::Clipboard::new().map_err(|err| err.to_string())?);
        }
        let Some(clipboard) = clipboard.as_mut() else {
            return Err("the clipboard isn't open".into());
        };
        clipboard
            .set_image(arboard::ImageData {
                width: width as usize,
                height: height as usize,
                bytes: rgba.into(),
            })
            .map_err(|err| err.to_string())
    }
}

impl Plugin for RenderPlugin {
    fn version(&self) -> String {
        env!("CARGO_PKG_VERSION").into()
    }

    fn commands(&self) -> Vec<Box<dyn PluginCommand<Plugin = Self>>> {
        vec![Box::new(command::Render)]
    }
}
