use std::env;
use std::fs;
use std::path::PathBuf;

const DEFAULT_JAR: &str = "/usr/share/scrcpy/scrcpy-server";
const DEFAULT_VERSION: &str = "5.0.1";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=SCRCPY_SERVER_JAR");
    println!("cargo:rerun-if-env-changed=SCRCPY_SERVER_VERSION");

    let jar_path = env::var_os("SCRCPY_SERVER_JAR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_JAR));

    let bytes = fs::read(&jar_path).unwrap_or_else(|err| {
        panic!(
            "could not read the scrcpy-server jar at {} ({err}).\n\
             Install scrcpy (sudo dnf install scrcpy) or point SCRCPY_SERVER_JAR at \
             a scrcpy 5.x scrcpy-server apk.",
            jar_path.display()
        )
    });
    if bytes.len() < 100_000 {
        panic!(
            "{} does not look like scrcpy-server ({} bytes)",
            jar_path.display(),
            bytes.len()
        );
    }

    let version = env::var("SCRCPY_SERVER_VERSION").unwrap_or_else(|_| DEFAULT_VERSION.to_string());

    // Warn when the installed scrcpy disagrees with the pinned server version:
    // the server rejects a client whose version string does not match its own.
    if let Ok(output) = std::process::Command::new("scrcpy")
        .arg("--version")
        .output()
    {
        if let Some(first) = String::from_utf8_lossy(&output.stdout).lines().next() {
            if let Some(installed) = first.split_whitespace().nth(1) {
                if installed != version {
                    println!(
                        "cargo:warning=scrcpy {installed} is installed but scrcpy-server {version} is embedded; set SCRCPY_SERVER_JAR/SCRCPY_SERVER_VERSION to matching values"
                    );
                }
            }
        }
    }
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let out_jar = out_dir.join("scrcpy-server.jar");
    fs::write(&out_jar, &bytes).expect("write scrcpy-server.jar to OUT_DIR");

    println!(
        "cargo:rustc-env=SCRCPY_SERVER_JAR_PATH={}",
        out_jar.display()
    );
    println!("cargo:rustc-env=SCRCPY_SERVER_VERSION={version}");
    println!("cargo:rustc-env=SCRCPY_SERVER_SIZE={}", bytes.len());
}
