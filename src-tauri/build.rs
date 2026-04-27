use std::path::PathBuf;

const CHROME_VERSION: &str = "132.0.6834.83";

fn main() {
    download_chromium_if_needed();
    tauri_build::build();
}

fn download_chromium_if_needed() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();

    let (platform, zip_name, binary_subpath) = match target_os.as_str() {
        "windows" => (
            "win64",
            "chrome-win64.zip",
            "chrome-win64/chrome.exe",
        ),
        "macos" if target_arch == "aarch64" => (
            "mac-arm64",
            "chrome-mac-arm64.zip",
            "chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
        ),
        "macos" => (
            "mac-x64",
            "chrome-mac-x64.zip",
            "chrome-mac-x64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
        ),
        _ => (
            "linux64",
            "chrome-linux64.zip",
            "chrome-linux64/chrome",
        ),
    };

    let out_dir = PathBuf::from("resources/chromium");
    let sentinel = out_dir.join(".downloaded");

    if sentinel.exists() {
        println!("cargo:rerun-if-changed=resources/chromium/.downloaded");
        return;
    }

    println!("cargo:warning=Downloading Chrome for Testing {CHROME_VERSION} for {platform}...");
    std::fs::create_dir_all(&out_dir).expect("failed to create resources/chromium");

    let url = format!(
        "https://storage.googleapis.com/chrome-for-testing-public/{CHROME_VERSION}/{platform}/{zip_name}"
    );

    let bytes = reqwest::blocking::get(&url)
        .expect("failed to download Chrome for Testing")
        .bytes()
        .expect("failed to read response bytes");

    let cursor = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor).expect("failed to open zip");

    let binary_leaf = binary_subpath.split('/').last().unwrap_or("");

    for i in 0..archive.len() {
        let mut file = archive.by_index(i).expect("zip entry error");
        let raw_path = file.name().to_owned();
        let target_path = out_dir.join(&raw_path);

        if file.is_dir() {
            std::fs::create_dir_all(&target_path).ok();
        } else {
            if let Some(parent) = target_path.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            let mut out = std::fs::File::create(&target_path).expect("failed to create file");
            std::io::copy(&mut file, &mut out).expect("failed to write file");

            #[cfg(unix)]
            if raw_path.ends_with(binary_leaf) {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&target_path, std::fs::Permissions::from_mode(0o755)).ok();
            }
        }
    }

    std::fs::write(&sentinel, CHROME_VERSION).expect("failed to write sentinel");
    println!("cargo:warning=Chrome for Testing ready at {out_dir:?}");
    println!("cargo:rerun-if-changed=resources/chromium/.downloaded");
}
