//! Runtime packaging metadata, installed by the package rather than baked into
//! the executable. Only inspect the running copy's prefix, never global paths.

use std::io::ErrorKind;
use std::path::Path;

const MANAGED: &str = "meshfox check-updates: this installation is managed by a package manager; use your package manager to update meshfox.";

/// `exe` must be canonicalized so a Homebrew bin symlink resolves into the
/// versioned Cellar prefix and does not pick up another installation's marker.
pub fn package_manager_message(exe: &Path) -> Option<String> {
    let bin = exe.parent()?;
    if bin.file_name()? != "bin" {
        return None;
    }
    let marker = bin.parent()?.join("share/meshfox/install-method");
    // Unlike exists()/is_file(), this distinguishes a missing marker from an
    // inaccessible one and recognizes dangling symlinks as managed installs.
    match std::fs::symlink_metadata(&marker) {
        Err(e) if e.kind() == ErrorKind::NotFound => return None,
        Err(_) => return Some(MANAGED.to_owned()),
        Ok(_) => {}
    }
    let method = std::fs::read_to_string(marker).unwrap_or_default();
    Some(match method.trim() {
        "homebrew" => format!("{MANAGED}\nRun: brew upgrade orofarne/tap/meshfox"),
        // A deb may have been installed without an apt repository, and a
        // pacman package may come from the AUR rather than an official repo.
        "deb" => format!("{MANAGED}\nUse apt or the source of your .deb package."),
        "pacman" => format!("{MANAGED}\nUse pacman or the tool used to install this package."),
        _ => MANAGED.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("mfx-update-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn markers_are_scoped_to_the_executable_prefix() {
        let scratch = Scratch::new();
        for prefix in [
            "usr",
            "usr/local",
            "home/user/.local",
            "Cellar/meshfox/1.2.3",
            "opt/meshfox",
        ] {
            let prefix = scratch.0.join(prefix);
            let marker = prefix.join("share/meshfox/install-method");
            let exe = prefix.join("bin/meshfox");
            assert_eq!(package_manager_message(&exe), None);
            std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
            std::fs::write(&marker, "homebrew\n").unwrap();
            assert!(package_manager_message(&exe)
                .unwrap()
                .contains("brew upgrade"));
        }
        assert_eq!(
            package_manager_message(&scratch.0.join("other/bin/meshfox")),
            None
        );
        // A standalone release archive has no bin/ prefix contract.
        assert_eq!(
            package_manager_message(&scratch.0.join("usr/meshfox")),
            None
        );
    }

    #[test]
    fn unknown_empty_invalid_and_unreadable_markers_are_managed() {
        let scratch = Scratch::new();
        let marker = scratch.0.join("share/meshfox/install-method");
        let exe = scratch.0.join("bin/meshfox");
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        for value in [b"".as_slice(), b"other-manager\n", b"\xff"] {
            std::fs::write(&marker, value).unwrap();
            assert_eq!(package_manager_message(&exe).as_deref(), Some(MANAGED));
        }
        std::fs::remove_file(&marker).unwrap();
        // Reading a directory as text fails even when the tests run as root.
        std::fs::create_dir(&marker).unwrap();
        assert_eq!(package_manager_message(&exe).as_deref(), Some(MANAGED));
    }

    #[test]
    fn deb_and_pacman_markers_provide_packaging_hints() {
        let scratch = Scratch::new();
        let marker = scratch.0.join("share/meshfox/install-method");
        let exe = scratch.0.join("bin/meshfox");
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        for (method, hint) in [("deb\n", "apt"), ("pacman\n", "pacman")] {
            std::fs::write(&marker, method).unwrap();
            let message = package_manager_message(&exe).unwrap();
            assert!(message.starts_with(MANAGED));
            assert!(message.contains(hint));
        }
    }

    #[cfg(unix)]
    #[test]
    fn dangling_marker_symlink_is_managed() {
        let scratch = Scratch::new();
        let marker = scratch.0.join("share/meshfox/install-method");
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink("missing", &marker).unwrap();
        assert_eq!(
            package_manager_message(&scratch.0.join("bin/meshfox")).as_deref(),
            Some(MANAGED)
        );
    }
}
