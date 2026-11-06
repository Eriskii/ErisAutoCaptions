//! Fetches Moonshine's static library, the speech engine and ONNX Runtime in one archive, and links
//! it in.

use std::path::Path;
use std::process::Command;
use std::{env, fs};

const VERSION: &str = "v0.1.6";
const ARCHIVE: &str = "moonshine-voice-macos-arm64.tar.gz";
const SHA256: &str = "809555a4e2c9a6972c0960a4cbf08b95121763ca17bce69f817adaac3b804fd0";

fn run(command: &mut Command) -> String {
    let output = command.output().unwrap_or_else(|error| panic!("{command:?}: {error}"));
    assert!(output.status.success(), "{command:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let out = env::var("OUT_DIR").unwrap();
    let lib = Path::new(&out).join(format!("moonshine-{VERSION}"));
    if !lib.join("moonshine-voice-macos-arm64/lib/libmoonshine.a").exists() {
        fs::create_dir_all(&lib).unwrap();
        let archive = lib.join(ARCHIVE);
        let url = format!("https://github.com/moonshine-ai/moonshine/releases/download/{VERSION}/{ARCHIVE}");
        run(Command::new("curl").args(["-fsSL", "--retry", "3", "-o"]).arg(&archive).arg(url));
        let sum = run(Command::new("shasum").args(["-a", "256"]).arg(&archive));
        assert_eq!(sum.split_whitespace().next(), Some(SHA256), "{ARCHIVE} isn't the expected file");
        run(Command::new("tar").arg("xzf").arg(&archive).arg("-C").arg(&lib));
        fs::remove_file(archive).unwrap();
    }
    println!("cargo:rustc-link-search=native={}/moonshine-voice-macos-arm64/lib", lib.display());
    println!("cargo:rustc-link-lib=static=moonshine");
    println!("cargo:rustc-link-lib=c++");
    // The library's OS version checks live in clang's runtime, which a Rust link leaves out.
    let runtime = run(Command::new("xcrun").args(["clang", "--print-runtime-dir"]));
    println!("cargo:rustc-link-search=native={}", runtime.trim());
    println!("cargo:rustc-link-lib=static=clang_rt.osx");
    for framework in ["Accelerate", "CoreFoundation", "CoreML", "Foundation"] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
}
