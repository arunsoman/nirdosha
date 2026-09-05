// The Rust "plugin" side, compiled as a staticlib -- a stand-in for a
// real Kind-A plugin's scalar-only builtin (rot13-shaped work would
// need a string ABI too; this isolates the *call* question first, the
// hard part codegen.rs's own doc comment names).
#[no_mangle]
pub extern "C" fn plugin_scale(x: i64) -> i64 {
    // Non-trivial enough that a real optimizer can't just fold the
    // whole loop away when this is called from a caller that varies x.
    x.wrapping_mul(2654435761i64).wrapping_add(1)
}
