// The native in-process insertion harness links the same AppKit/Swift runtime
// dependencies as the macOS adapter's own test binaries.
include!("../platform-macos/build.rs");
