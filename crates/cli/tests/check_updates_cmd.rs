//! Run the actual binary from an installation prefix, including its public
//! symlink. The packaging guard must run before release/network/TTY handling.

mod common;

#[test]
fn package_managed_binary_refuses_self_update_through_a_symlink_even_with_yes() {
    let dir = common::unique_dir();
    let prefix = dir.join("Cellar/meshfox/1.2.3");
    let bin = prefix.join("bin/meshfox");
    let marker = prefix.join("share/meshfox/install-method");
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_meshfox"), &bin).unwrap();
    let link = dir.join("meshfox");
    std::os::unix::fs::symlink(&bin, &link).unwrap();

    for value in ["homebrew\n", "deb\n", "pacman\n", "", "unknown\n"] {
        std::fs::write(&marker, value).unwrap();
        for yes in [false, true] {
            let mut command = std::process::Command::new(&link);
            command.env("HOME", dir.join("home")).arg("check-updates");
            if yes {
                command.arg("--yes");
            }
            let output = command.output().unwrap();
            assert!(output.status.success(), "{output:?}");
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(stdout.contains("managed by a package manager"), "{stdout}");
            assert!(
                !stdout.contains("wasn't made from a release tag"),
                "{stdout}"
            );
            if value.trim() == "homebrew" {
                assert!(
                    stdout.contains("brew upgrade orofarne/tap/meshfox"),
                    "{stdout}"
                );
            }
            assert!(output.stderr.is_empty(), "{output:?}");
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}
