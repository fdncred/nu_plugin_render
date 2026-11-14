//! The kitty graphics protocol: image bands placed at the cursor.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;

/// Input bytes per escape code: 3072 bytes are 4096 base64 characters, the
/// protocol's limit for one chunk.
const CHUNK: usize = 3072;

/// How a band's pixels reach the terminal.
pub enum Payload {
    /// PNG bytes, inline in base64. Works everywhere, but the terminal has
    /// to decode the PNG.
    Png(Vec<u8>),
    /// Raw RGBA in a temporary file, which the terminal reads and deletes.
    /// Only for a terminal on this machine, and much faster: no PNG to
    /// encode or decode, and no base64.
    File {
        path: PathBuf,
        width: u32,
        height: u32,
    },
}

/// A new temporary file name the terminal will read and delete. Terminals
/// only delete files in the temporary directory whose names contain
/// `tty-graphics-protocol`.
pub fn temp_path() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let next = NEXT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("{TEMP_PREFIX}{}-{next}", std::process::id()))
}

/// The start of every temporary file name this plugin writes.
const TEMP_PREFIX: &str = "tty-graphics-protocol-nu_plugin_render-";

/// How old a temporary file must be before [`sweep_temp_files`] takes it.
/// A terminal reads its files within milliseconds of the output reaching
/// it, so an older one belongs to output that never got there, such as
/// `render | ignore`.
const STALE: std::time::Duration = std::time::Duration::from_secs(60);

/// Delete this plugin's temporary image files that no terminal took.
pub fn sweep_temp_files() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let ours = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(TEMP_PREFIX));
        let stale = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > STALE);
        if ours && stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Write raw RGBA pixels to a temporary file for the terminal.
///
/// # Errors
///
/// Fails when the file can't be written.
pub fn temp_file(rgba: &[u8], width: u32, height: u32) -> Result<Payload, String> {
    let path = temp_path();
    std::fs::write(&path, rgba).map_err(|err| format!("writing {}: {err}", path.display()))?;
    Ok(Payload::File {
        path,
        width,
        height,
    })
}

/// Append the codes that show `payload` over `columns` × `rows` cells at
/// the start of the cursor's line.
///
/// The image goes up with the cursor left where it is (`C=1`), after making
/// room with newlines, so the screen has already scrolled and every
/// terminal leaves the cursor in the same place. After it, the cursor moves
/// below the image, except after the `last` band, where it stays on the
/// image's last row because nushell ends the output with a newline.
pub fn place(payload: &Payload, columns: u32, rows: u32, last: bool, out: &mut String) {
    let rows = rows.max(1);
    for _ in 0..rows {
        out.push('\n');
    }
    let _ = write!(out, "\x1b[{rows}A\r");
    let keys = format!("a=T,q=2,C=1,c={columns},r={rows}");
    match payload {
        Payload::Png(png) => {
            let chunks = png.chunks(CHUNK);
            let final_chunk = chunks.len().saturating_sub(1);
            for (index, chunk) in chunks.enumerate() {
                let more = u8::from(index != final_chunk);
                if index == 0 {
                    let _ = write!(out, "\x1b_G{keys},f=100,m={more};");
                } else {
                    let _ = write!(out, "\x1b_Gm={more};");
                }
                BASE64.encode_string(chunk, out);
                out.push_str("\x1b\\");
            }
        }
        Payload::File {
            path,
            width,
            height,
        } => {
            let _ = write!(out, "\x1b_G{keys},f=32,s={width},v={height},t=t;");
            BASE64.encode_string(path.as_os_str().as_encoded_bytes(), out);
            out.push_str("\x1b\\");
        }
    }
    let down = if last { rows - 1 } else { rows };
    if down > 0 {
        let _ = write!(out, "\x1b[{down}B");
    }
    out.push('\r');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_png_into_chunks_and_moves_below() {
        let png = Payload::Png(vec![0u8; CHUNK + 1]);
        let mut out = String::new();
        place(&png, 10, 3, false, &mut out);
        assert!(out.starts_with("\n\n\n\x1b[3A\r\x1b_Ga=T,q=2,C=1,c=10,r=3,f=100,m=1;"));
        assert!(out.contains("\x1b_Gm=0;"));
        assert!(out.ends_with("\x1b\\\x1b[3B\r"));
    }

    #[test]
    fn last_band_stays_on_its_last_row() {
        let mut out = String::new();
        place(&Payload::Png(vec![1, 2, 3]), 4, 1, true, &mut out);
        assert!(out.ends_with("\x1b\\\r"));
    }

    #[test]
    fn names_a_temporary_file_in_one_code() {
        let file = Payload::File {
            path: PathBuf::from("/tmp/tty-graphics-protocol-x"),
            width: 16,
            height: 32,
        };
        let mut out = String::new();
        place(&file, 1, 1, true, &mut out);
        let name = BASE64.encode("/tmp/tty-graphics-protocol-x");
        assert_eq!(
            out,
            format!("\n\x1b[1A\r\x1b_Ga=T,q=2,C=1,c=1,r=1,f=32,s=16,v=32,t=t;{name}\x1b\\\r")
        );
    }
}
