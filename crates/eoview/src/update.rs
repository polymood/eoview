//! Automatic updates. At the start, a thread reads the version of the latest release of eoview on GitHub.
//! If it is newer, the thread downloads the executable of this system, checks its signature (ed25519, with
//! the key of the releases) and puts it in the place of the executable that runs. The new version runs at
//! the next start. Only the executables of the releases (built with EOVIEW_RELEASE) update themselves, not a
//! build from the source. Preference `no_update`.
use std::path::Path;
use std::sync::{Arc, Mutex};

/// The releases. EOVIEW_UPDATE_URL: an other place with the same layout (a test).
const RELEASES: &str = "https://github.com/polymood/eoview/releases";

/// The public key of the releases. The private key (secret EOVIEW_SIGNING_KEY of the repository) signs the
/// executables in .github/workflows/release.yml.
const KEY: [u8; 32] = [242, 191, 183, 190, 43, 116, 25, 137, 123, 173, 12, 145, 255, 206, 143, 182, 95, 110, 107, 49, 92, 245, 216, 186, 234, 48, 171, 160, 109, 0, 42, 247];

/// The result of the update for the interface: the version that is installed, or the error. None: the
/// check runs, or eoview is the latest version.
pub type Status = Arc<Mutex<Option<Result<String, String>>>>;

/// The update runs for this executable: a release, or a test with EOVIEW_UPDATE_URL.
pub fn enabled() -> bool {
    option_env!("EOVIEW_RELEASE").is_some() || std::env::var_os("EOVIEW_UPDATE_URL").is_some()
}

/// The name of the executable of this system in a release.
pub fn asset() -> String {
    format!("eoview-{}-{}{}", std::env::consts::OS, std::env::consts::ARCH, std::env::consts::EXE_SUFFIX)
}

/// Start the update in a thread. `wake` shows the result in the interface.
pub fn start(wake: crate::Wake) -> Status {
    let st = Status::default();
    let s = st.clone();
    let _ = std::thread::Builder::new().name("eoview-update".into()).spawn(move || {
        let r = std::env::current_exe().map_err(|e| e.to_string()).and_then(|exe| update(&exe));
        if let Err(e) = &r {
            eprintln!("update: {e}");
        }
        if let Some(r) = r.transpose() {
            *s.lock().unwrap() = Some(r);
            wake();
        }
    });
    st
}

/// Update the executable `exe` to the latest release. Some(version): the new version is installed.
fn update(exe: &Path) -> Result<Option<String>, String> {
    // The executable of the last update (see `replace`).
    let _ = std::fs::remove_file(exe.with_extension("old"));
    let base = std::env::var("EOVIEW_UPDATE_URL").unwrap_or_else(|_| RELEASES.into());
    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!("eoview/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(|e| e.to_string())?;
    let get = |url: String| -> Result<Vec<u8>, String> {
        let r = client.get(&url).send().and_then(|r| r.error_for_status()).map_err(|e| format!("{url}: {e}"))?;
        r.bytes().map(|b| b.to_vec()).map_err(|e| format!("{url}: {e}"))
    };
    let v = String::from_utf8_lossy(&get(format!("{base}/latest/download/version.txt"))?).trim().to_string();
    if !newer(&v, env!("CARGO_PKG_VERSION")) {
        return Ok(None);
    }
    let name = asset();
    let bin = get(format!("{base}/download/v{v}/{name}"))?;
    let sig = get(format!("{base}/download/v{v}/{name}.sig"))?;
    use aws_lc_rs::signature::{ED25519, UnparsedPublicKey};
    UnparsedPublicKey::new(&ED25519, KEY).verify(&bin, &sig).map_err(|_| format!("{name} {v}: the signature is not valid"))?;
    replace(exe, &bin).map_err(|e| format!("{}: {e}", exe.display()))?;
    Ok(Some(v))
}

/// Version `a` is after version `b` (numbers with dots). An other text in `a` is not a version.
fn newer(a: &str, b: &str) -> bool {
    let p = |s: &str| s.split('.').map(|x| x.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    !a.is_empty() && a.chars().all(|c| c.is_ascii_digit() || c == '.') && p(a) > p(b)
}

/// Put `bin` in the place of the executable `exe`. A program that runs cannot be written, but it can be
/// renamed (also on Windows): it goes to `exe.old`, and the next update deletes it.
fn replace(exe: &Path, bin: &[u8]) -> std::io::Result<()> {
    let (new, old) = (exe.with_extension("new"), exe.with_extension("old"));
    let r = (|| {
        std::fs::write(&new, bin)?;
        #[cfg(unix)]
        std::fs::set_permissions(&new, <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755))?;
        std::fs::rename(exe, &old)?;
        std::fs::rename(&new, exe).inspect_err(|_| drop(std::fs::rename(&old, exe)))
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(&new);
    }
    r
}

#[cfg(test)]
mod tests {
    #[test]
    fn versions_and_replace() {
        use super::newer;
        assert!(newer("0.2.0", "0.1.0") && newer("0.10.0", "0.9.9") && newer("1.0", "0.9.9"));
        assert!(!newer("0.1.0", "0.1.0") && !newer("0.0.9", "0.1.0") && !newer("<html>", "0.1.0") && !newer("", "0.1.0"));
        let d = std::env::temp_dir().join(format!("eoview-update-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let exe = d.join("eoview.exe");
        std::fs::write(&exe, b"old").unwrap();
        super::replace(&exe, b"new").unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        assert_eq!(std::fs::read(d.join("eoview.old")).unwrap(), b"old");
        assert!(!d.join("eoview.new").exists());
        std::fs::remove_dir_all(&d).unwrap();
    }
}
