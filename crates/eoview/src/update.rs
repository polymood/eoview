//! Updates. At the start, a thread reads the version of the latest release of eoview on GitHub. If it is
//! newer, the interface asks the user (`App::update_ui`). On "Update and restart", a thread downloads the
//! executable of this system, checks its signature (ed25519, with the key of the releases, see `check`) and
//! puts it in the place of the executable that runs. Then eoview starts the new version with the open workspace, and stops.
//! Only the executables of the releases (built with EOVIEW_RELEASE) update themselves, not a build from the
//! source. Preference `no_update`.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The releases. EOVIEW_UPDATE_URL: an other place with the same layout (a test).
const RELEASES: &str = "https://github.com/polymood/eoview/releases";

/// The changes of each version.
pub const CHANGELOG: &str = "https://github.com/polymood/eoview/blob/main/CHANGELOG.md";

/// The public key of the releases. The private key (secret EOVIEW_SIGNING_KEY of the repository) signs the
/// executables in .github/workflows/release.yml.
const KEY: [u8; 32] = [242, 191, 183, 190, 43, 116, 25, 137, 123, 173, 12, 145, 255, 206, 143, 182, 95, 110, 107, 49, 92, 245, 216, 186, 234, 48, 171, 160, 109, 0, 42, 247];

/// The largest download. An executable of a release has about 30 MB.
const MAX_EXE: u64 = 256 << 20;
/// The largest download of a version or a signature.
const MAX_TEXT: u64 = 4096;

/// The state of the update for the interface. None: the check runs, or eoview is the latest version.
#[derive(Clone)]
pub enum State {
    /// A newer version: the interface asks the user.
    Available(String),
    /// The download of this version runs.
    Installing(String),
    /// The new executable is in place: start it.
    Installed(PathBuf),
    /// The check failed (for example, no network). The About page shows it.
    Failed(String),
    /// The install that the user asked for failed.
    InstallFailed(String),
}

pub type Status = Arc<Mutex<Option<State>>>;

/// The update runs for this executable: a release, or a test with EOVIEW_UPDATE_URL.
pub fn enabled() -> bool {
    option_env!("EOVIEW_RELEASE").is_some() || std::env::var_os("EOVIEW_UPDATE_URL").is_some()
}

/// The name of the executable of this system in a release.
pub fn asset() -> String {
    format!("eoview-{}-{}{}", std::env::consts::OS, std::env::consts::ARCH, std::env::consts::EXE_SUFFIX)
}

fn base() -> String {
    std::env::var("EOVIEW_UPDATE_URL").unwrap_or_else(|_| RELEASES.into())
}

/// The bytes at `url`. More than `max` bytes is an error: the download stops there.
fn get(url: String, max: u64) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!("eoview/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(|e| e.to_string())?;
    let err = |e: &dyn std::fmt::Display| eo_core::redact(&format!("{url}: {e}"));
    let r = client.get(&url).send().and_then(|r| r.error_for_status()).map_err(|e| err(&e))?;
    let mut b = Vec::new();
    r.take(max + 1).read_to_end(&mut b).map_err(|e| err(&e))?;
    if b.len() as u64 > max {
        return Err(err(&format!("more than {max} bytes")));
    }
    Ok(b)
}

/// Run `f` in a thread and put its state in `st`. `wake` shows it in the interface.
fn spawn(st: &Status, wake: crate::Wake, f: impl FnOnce() -> Option<State> + Send + 'static) {
    let s = st.clone();
    let _ = std::thread::Builder::new().name("eoview-update".into()).spawn(move || {
        let r = f();
        *s.lock().unwrap() = r;
        wake();
    });
}

/// Start the check of the latest version in a thread.
pub fn start(wake: crate::Wake) -> Status {
    let st = Status::default();
    spawn(&st, wake, || {
        // The executable of the last update (see `replace`).
        if let Ok(exe) = std::env::current_exe() {
            let _ = std::fs::remove_file(exe.with_extension("old"));
        }
        match get(format!("{}/latest/download/version.txt", base()), MAX_TEXT) {
            Ok(v) => Some(String::from_utf8_lossy(&v).trim().to_string()).filter(|v| newer(v, env!("CARGO_PKG_VERSION"))).map(State::Available),
            Err(e) => {
                eprintln!("update: {e}");
                Some(State::Failed(e))
            }
        }
    });
    st
}

/// Download version `v`, check it and put it in the place of the executable that runs.
pub fn install(st: &Status, v: String, wake: crate::Wake) {
    *st.lock().unwrap() = Some(State::Installing(v.clone()));
    spawn(st, wake, move || {
        let r = std::env::current_exe().map_err(|e| e.to_string()).and_then(|exe| {
            let name = asset();
            let bin = get(format!("{}/download/v{v}/{name}", base()), MAX_EXE)?;
            let sig = get(format!("{}/download/v{v}/{name}.version.sig", base()), MAX_TEXT)?;
            check(&KEY, &v, &name, &bin, &sig)?;
            replace(&exe, &bin).map_err(|e| format!("{}: {e}", exe.display()))?;
            Ok(exe)
        });
        Some(r.map_or_else(State::InstallFailed, State::Installed))
    });
}

/// The text that the release job signs for executable `name` of version `v` (file `name.version.sig`). The
/// signature of the version and of the name: an older release, or the executable of another system, cannot
/// come as an update. The release also has `name.sig`, the signature of the executable only, for the
/// versions 0.2.x and older install scripts.
fn signed_text(v: &str, name: &str, bin: &[u8]) -> String {
    let h = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, bin);
    format!("eoview {v} {name} {}", h.as_ref().iter().map(|b| format!("{b:02x}")).collect::<String>())
}

/// Check the executable `bin` of version `v`: `v` is newer than this version, and `sig` is the signature of
/// `signed_text` with the private key of `key`.
fn check(key: &[u8], v: &str, name: &str, bin: &[u8], sig: &[u8]) -> Result<(), String> {
    if !newer(v, env!("CARGO_PKG_VERSION")) {
        return Err(format!("{name} {v}: not newer than version {}", env!("CARGO_PKG_VERSION")));
    }
    use aws_lc_rs::signature::{ED25519, UnparsedPublicKey};
    UnparsedPublicKey::new(&ED25519, key).verify(signed_text(v, name, bin).as_bytes(), sig).map_err(|_| format!("{name} {v}: the signature is not valid"))
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

    #[test]
    fn signature_of_the_version() {
        use super::{check, signed_text};
        use aws_lc_rs::signature::{Ed25519KeyPair, KeyPair};
        assert_eq!(signed_text("1.0", "a", b"abc"), "eoview 1.0 a ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let k = Ed25519KeyPair::generate().unwrap();
        let key = k.public_key().as_ref().to_vec();
        let sig = |v: &str, name: &str| k.sign(signed_text(v, name, b"exe").as_bytes()).as_ref().to_vec();
        assert!(check(&key, "99.0.0", "a", b"exe", &sig("99.0.0", "a")).is_ok());
        // An other version, an other system, an other executable, the old signature of the executable only.
        assert!(check(&key, "99.0.1", "a", b"exe", &sig("99.0.0", "a")).is_err());
        assert!(check(&key, "99.0.0", "b", b"exe", &sig("99.0.0", "a")).is_err());
        assert!(check(&key, "99.0.0", "a", b"exf", &sig("99.0.0", "a")).is_err());
        assert!(check(&key, "99.0.0", "a", b"exe", k.sign(b"exe").as_ref()).is_err());
        // A valid signature of an older version: not an update.
        assert!(check(&key, "0.0.1", "a", b"exe", &sig("0.0.1", "a")).is_err());
    }
}
