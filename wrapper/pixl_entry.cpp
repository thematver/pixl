// PIXL plugin entry for clap-wrapper's clap-first assembly.
//
// `clap_entry` is defined in Rust (nih-plug's `nih_export_clap!`) inside libpixl.a.
// Taking its address here guarantees the archive member that defines it is linked
// into every format (CLAP, VST3, AUv2), and keeps the symbol exported from the CLAP.

#include <clap/clap.h>

extern "C" {
extern const clap_plugin_entry clap_entry;
}

__attribute__((used, visibility("default"))) const void *pixl_clap_entry_anchor = &clap_entry;
