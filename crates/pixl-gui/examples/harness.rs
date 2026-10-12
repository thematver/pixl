//! `cargo run -p pixl-gui --release --offline --example harness`
use pixl_gui::{EditorApp,MockBridge};
use std::sync::Arc;
fn main(){
    #[cfg(target_os="macos")]
    pixl_gfx::run_standalone(Box::new(EditorApp::new(Arc::new(MockBridge::new())))).expect("standalone editor");
    #[cfg(not(target_os="macos"))]
    {let _=EditorApp::new(Arc::new(MockBridge::new()));eprintln!("Native harness requires macOS; use snapshots on this platform.");}
}
