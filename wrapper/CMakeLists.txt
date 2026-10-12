# PIXL — plugin format assembly (CLAP / VST3 / AUv2) via clap-wrapper "clap-first".
#
# The entire plugin lives in the Rust static library `libpixl.a` (crates/pixl-plugin),
# which exports the CLAP `clap_entry` symbol through nih-plug. clap-wrapper then
# assembles self-contained CLAP, VST3 and AUv2 bundles that statically link it.
#
# Invoked through `make build`, which builds the Rust library first and passes
# PIXL_RUST_LIB.

cmake_minimum_required(VERSION 3.21)

set(CMAKE_OSX_DEPLOYMENT_TARGET "11.0" CACHE STRING "Minimum macOS version")
if (NOT DEFINED CMAKE_OSX_ARCHITECTURES)
    set(CMAKE_OSX_ARCHITECTURES "arm64" CACHE STRING "Target architectures")
endif()

project(PIXL VERSION 1.0.0 LANGUAGES C CXX OBJC OBJCXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

set(PIXL_RUST_LIB "${CMAKE_CURRENT_SOURCE_DIR}/../target/release/libpixl.a" CACHE FILEPATH "Path to the Rust static library")
set(PIXL_BUNDLE_ID "audio.pixl.PIXL" CACHE STRING "Bundle identifier prefix")
set(PIXL_OUTPUT_DIR "${CMAKE_BINARY_DIR}/out" CACHE PATH "Where the assembled bundles are written")

if (NOT EXISTS "${PIXL_RUST_LIB}")
    message(FATAL_ERROR "Rust static library not found at ${PIXL_RUST_LIB}. Run `make build`.")
endif()

set(CLAP_WRAPPER_DOWNLOAD_DEPENDENCIES TRUE CACHE BOOL "" FORCE)
set(CLAP_WRAPPER_BUILD_AUV2 TRUE CACHE BOOL "" FORCE)
set(CLAP_WRAPPER_COPY_AFTER_BUILD FALSE CACHE BOOL "" FORCE)

include(FetchContent)
FetchContent_Declare(clap-wrapper
    GIT_REPOSITORY https://github.com/free-audio/clap-wrapper.git
    GIT_TAG v0.16.0
    GIT_SHALLOW TRUE)
FetchContent_MakeAvailable(clap-wrapper)

add_library(pixl-rust STATIC IMPORTED GLOBAL)
set_target_properties(pixl-rust PROPERTIES IMPORTED_LOCATION "${PIXL_RUST_LIB}")

add_library(pixl-impl INTERFACE)
target_link_libraries(pixl-impl INTERFACE
    pixl-rust
    clap
    "-framework Foundation"
    "-framework CoreFoundation"
    "-framework AppKit"
    "-framework Metal"
    "-framework CoreGraphics"
    "-framework QuartzCore"
    "-framework CoreAudio"
    "-framework Security"
    "-framework SystemConfiguration"
    "-liconv"
    "-lobjc")

make_clapfirst_plugins(
    TARGET_NAME PIXL
    IMPL_TARGET pixl-impl
    OUTPUT_NAME "PIXL"
    ENTRY_SOURCE "${CMAKE_CURRENT_SOURCE_DIR}/pixl_entry.cpp"

    BUNDLE_IDENTIFIER "${PIXL_BUNDLE_ID}"
    BUNDLE_VERSION "${PROJECT_VERSION}"

    COPY_AFTER_BUILD FALSE
    PLUGIN_FORMATS CLAP VST3 AUV2
    ASSET_OUTPUT_DIRECTORY "${PIXL_OUTPUT_DIR}"

    AUV2_MANUFACTURER_NAME "PIXL Audio"
    AUV2_MANUFACTURER_CODE "PXLa"
    AUV2_SUBTYPE_CODE "PIXL"
    AUV2_INSTRUMENT_TYPE "aumu"
)
