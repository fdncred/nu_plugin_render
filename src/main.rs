//! `nu_plugin_render`: draws nushell tables as images in the terminal.

use nu_plugin::{MsgPackSerializer, serve_plugin};
use nu_plugin_render::RenderPlugin;

fn main() {
    serve_plugin(&RenderPlugin::default(), MsgPackSerializer);
}
