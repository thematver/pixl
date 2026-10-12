// Private PIXL CLAP -> AUv2 bridge. Keep in sync with entry.rs.
#pragma once
#include <clap/clap.h>

struct PixlFactoryPresetInfo {
    int32_t number;
    char name[256];
};
struct PixlFactoryPresets {
    uint32_t (*count)(const clap_plugin_t *);
    bool (*get)(const clap_plugin_t *, uint32_t index, PixlFactoryPresetInfo *);
    bool (*load)(const clap_plugin_t *, int32_t number);
    bool (*current)(const clap_plugin_t *, PixlFactoryPresetInfo *);
};
constexpr const char *PIXL_FACTORY_PRESETS_ID = "audio.pixl.factory-presets/1";
