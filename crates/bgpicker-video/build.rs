fn main() {
    println!("cargo:rerun-if-changed=ffi/ffmpeg_shim.h");
    println!("cargo:rerun-if-changed=ffi/ffmpeg_shim.c");

    let libs = [
        "libavformat",
        "libavcodec",
        "libswscale",
        "libavutil",
        "libdrm",
    ];

    let mut build = cc::Build::new();
    build
        .file("ffi/ffmpeg_shim.c")
        .include("ffi")
        .flag_if_supported("-Wall")
        .flag_if_supported("-Wextra")
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-std=c11");

    for lib in libs {
        let probe = pkg_config::Config::new()
            .cargo_metadata(true)
            .probe(lib)
            .unwrap_or_else(|e| panic!("failed to probe {lib} via pkg-config: {e}"));
        for inc in probe.include_paths {
            build.include(inc);
        }
        for (key, val) in probe.defines {
            match val {
                Some(v) => build.define(&key, Some(v.as_str())),
                None => build.define(&key, None),
            };
        }
    }

    build.compile("bgpicker_ffmpeg_shim");
}
