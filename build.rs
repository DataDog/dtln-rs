#[cfg(target_os = "windows")]
fn main() {
    // NOP on windows
}

#[cfg(target_os = "macos")]
fn main() {
    use std::{env, process::Command};

    use build_target::Arch;

    // Use prebuilts for macos if they are available.
    let target_arch = build_target::target_arch().unwrap();
    match target_arch {
        Arch::WASM32 => {
            Command::new("tar")
                .arg("-xjf")
                .arg("./tflite/tflite-prebuilt.wasm.tar.bz2")
                .arg("-C")
                .arg("./tflite/")
                .status()
                .unwrap();
        }
        Arch::AARCH64 => {
            Command::new("tar")
                .arg("-xjf")
                .arg("./tflite/tflite-prebuilt.osx.arm64.tar.bz2")
                .arg("-C")
                .arg("./tflite/")
                .status()
                .unwrap();
        }
        Arch::X86_64 => {
            Command::new("tar")
                .arg("-xjf")
                .arg("./tflite/tflite-prebuilt.osx.x64.tar.bz2")
                .arg("-C")
                .arg("./tflite/")
                .status()
                .unwrap();
        }
        _ => {
            // Try using Conan.
            Command::new("cmake")
                .current_dir("tflite")
                .arg(".")
                .arg("-DCMAKE_BUILD_TYPE=Release")
                .status()
                .expect("Failed to run cmake");
        }
    }

    let root_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rustc-link-search=native={}/tflite/lib/", root_dir);

    // Link to all archives in lib directory, generated by conan.
    std::fs::read_dir(std::format!("{}/tflite/lib", root_dir))
        .unwrap()
        .for_each(|entry| {
            let path = entry.unwrap().path();
            let extension = path.extension();
            match extension {
                Some(ext) if ext == "a" => {
                    let lib_name = path.file_stem().unwrap().to_str().unwrap();
                    if let Some(lib_name) = lib_name.strip_prefix("lib") {
                        println!("cargo:rustc-link-lib=dylib={}", lib_name);
                    }
                }
                _ => {}
            };
        });
}
