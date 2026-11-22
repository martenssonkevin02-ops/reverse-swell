//! Runs the plugin as a standalone app (used for testing the GUI).
use nih_plug::prelude::*;
use reverse_swell::ReverseSwell;

fn main() {
    nih_export_standalone::<ReverseSwell>();
}
