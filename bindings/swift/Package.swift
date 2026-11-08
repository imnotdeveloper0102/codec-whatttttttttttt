// swift-tools-version:5.9
import PackageDescription

// SwiftPM package for CodecWhat (Phase 10). The Swift code is a
// thin wrapper over the C ABI; the C header is vendored in
// Sources/CCodecWhat (a copy of
// crates/codec-whatttttttttttt-c/include/codec_whatttttttttttt.h —
// CI checks the two stay identical) and the Rust static library
// is linked from target/release, so build the C crate first:
//   cargo build --release -p codec-whatttttttttttt-c
// The unsafe linker flags are only valid for this root package;
// the package is not published as a dependency.

let package = Package(
    name: "CodecWhat",
    products: [
        .library(name: "CodecWhat", targets: ["CodecWhat"]),
    ],
    targets: [
        .systemLibrary(
            name: "CCodecWhat",
            path: "Sources/CCodecWhat"
        ),
        .target(
            name: "CodecWhat",
            dependencies: ["CCodecWhat"],
            linkerSettings: [
                // The static archive is passed by path, not with
                // -l: target/release also holds the cdylib .so, and
                // -l would prefer it, leaving a runtime dependency
                // on the shared library.
                .unsafeFlags([
                    "../../target/release/libcodec_whatttttttttttt.a",
                    "-lm", "-lpthread", "-ldl", "-lgcc_s",
                ]),
            ]
        ),
        .testTarget(
            name: "CodecWhatTests",
            // The tests assert on the C error codes and codec ids,
            // so they import the C module directly.
            dependencies: ["CodecWhat", "CCodecWhat"]
        ),
    ]
)
