//! The installer copies the rustup binary and leaves shell rc files and PATH alone.

const INIT_NONE: [&str; 4] = ["rustup-init", "-y", "--default-toolchain", "none"];

#[cfg(unix)]
mod unix {
    use std::fs;

    use rustup::{
        test::{CliTestContext, Scenario},
        utils::raw,
    };

    use super::INIT_NONE;

    #[tokio::test]
    async fn install_and_uninstall_leave_shell_rc_files_alone() {
        let cx = CliTestContext::new(Scenario::Empty).await;
        let profile = cx.config.homedir.join(".profile");
        let bashrc = cx.config.homedir.join(".bashrc");
        raw::write_file(&profile, "echo hi\n").unwrap();

        cx.config.expect(&INIT_NONE).await.is_ok();

        assert_eq!(fs::read_to_string(&profile).unwrap(), "echo hi\n");
        assert!(!bashrc.exists());
        assert!(!cx.config.cargodir.join("env").exists());
        assert!(
            cx.config
                .cargodir
                .join(format!("bin/rustup{}", std::env::consts::EXE_SUFFIX))
                .exists()
        );
        assert!(
            !cx.config
                .cargodir
                .join(format!("bin/cargo{}", std::env::consts::EXE_SUFFIX))
                .exists()
        );

        cx.config
            .expect(["rustup", "self", "uninstall", "-y"])
            .await
            .is_ok();
        assert_eq!(fs::read_to_string(&profile).unwrap(), "echo hi\n");
    }
}

#[cfg(windows)]
mod windows {
    use rustup::test::{CliTestContext, Scenario, get_path};

    use super::INIT_NONE;

    #[tokio::test]
    async fn install_and_uninstall_leave_user_path_alone() {
        let cx = CliTestContext::new(Scenario::Empty).await;
        let test_id = &cx.config.test_registry_id;
        assert!(get_path(test_id).unwrap().is_none());

        cx.config.expect(&INIT_NONE).await.is_ok();
        assert!(get_path(test_id).unwrap().is_none());

        cx.config
            .expect(["rustup", "self", "uninstall", "-y"])
            .await
            .is_ok();
        assert!(get_path(test_id).unwrap().is_none());
    }
}
