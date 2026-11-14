use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let engine_dir = manifest.join("../engine").canonicalize().unwrap();
    let core_include = manifest.join("../../core/include").canonicalize().unwrap();
    println!("cargo:rerun-if-changed={}", engine_dir.display());
    println!("cargo:rerun-if-changed={}", core_include.display());
    println!("cargo:rerun-if-changed=build.rs");
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    if target_os != "macos" {
        let engine_dir = PathBuf::from(engine_dir.to_str().unwrap().trim_start_matches(r"\\?\"));
        let core_include = PathBuf::from(core_include.to_str().unwrap().trim_start_matches(r"\\?\"));
        let vulkan_include = engine_dir.join("vulkan/include");
        cc::Build::new().file(engine_dir.join("stb/stb_image.c")).warnings(false).compile("stb_image");
        let mut engine = cc::Build::new();
        engine.cpp(true).std("c++17").include(&engine_dir).include(&core_include).include(&vulkan_include).warnings(true);
        if target_os == "windows" {
            engine.flag("/utf-8").define("NOMINMAX", None);
        } else {
            engine.extra_warnings(true).flag("-Wno-unused-parameter").flag("-Wno-missing-field-initializers");
        }
        engine.file(engine_dir.join("engine_vk.cpp")).file(engine_dir.join("renderer_vk.cpp")).file(engine_dir.join("kqueue_compat.cpp")).file(engine_dir.join(if target_os == "windows" { "window_win32.cpp" } else { "window_x11.cpp" })).compile("hikari_engine");
        if target_os == "windows" {
            let target = env::var("TARGET").unwrap();
            let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
            let lib_exe = cc::windows_registry::find_tool(&target, "lib.exe").expect("lib.exe from visual studio build tools");
            let machine = if env::var("CARGO_CFG_TARGET_ARCH").unwrap() == "aarch64" { "/machine:arm64" } else { "/machine:x64" };
            let status = lib_exe.to_command().arg(format!("/def:{}", engine_dir.join("vulkan/vulkan-1.def").display())).arg(machine).arg(format!("/out:{}", out_dir.join("vulkan-1.lib").display())).arg("/nologo").status().unwrap();
            assert!(status.success(), "lib.exe failed to make vulkan-1.lib");
            println!("cargo:rustc-link-search=native={}", out_dir.display());
            for library in ["vulkan-1", "user32", "d3d11", "dxgi"] {
                println!("cargo:rustc-link-lib=dylib={}", library);
            }
        } else {
            println!("cargo:rustc-link-lib=dylib:+verbatim=libvulkan.so.1");
            println!("cargo:rustc-link-lib=dylib=X11");
        }
        return;
    }
    let syphon_dir = engine_dir.join("syphon");
    let mut engine = cc::Build::new();
    engine.cpp(true).std("c++17").flag("-fobjc-arc").flag("-Wno-c23-extensions").flag(format!("--embed-dir={}", engine_dir.display())).include(&engine_dir).include(&syphon_dir).include(&core_include).warnings(true).extra_warnings(true);
    for flag in ["-Wno-unused-parameter", "-Wno-missing-field-initializers"] {
        engine.flag(flag);
    }
    engine.file(engine_dir.join("engine.mm")).file(engine_dir.join("renderer_metal.mm")).compile("hikari_engine");
    let syphon_sources: Vec<PathBuf> = fs::read_dir(&syphon_dir).unwrap().map(|entry| entry.unwrap().path()).filter(|path| matches!(path.extension().and_then(OsStr::to_str), Some("m" | "c"))).collect();
    cc::Build::new()
        .flag("-fobjc-arc")
        .flag("-Wno-deprecated-declarations")
        .flag("-include")
        .flag(syphon_dir.join("Syphon_Prefix.pch").to_str().unwrap())
        .include(&syphon_dir)
        .warnings(false)
        .files(&syphon_sources)
        .compile("syphon");
    for framework in ["Metal", "Foundation", "ImageIO", "CoreGraphics", "AppKit", "QuartzCore", "IOSurface"] {
        println!("cargo:rustc-link-lib=framework={}", framework);
    }
}
