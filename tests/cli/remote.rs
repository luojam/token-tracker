use super::{command, successful_report};
use crate::support::TempTree;
use std::fs;

#[test]
fn remote_auth_uses_the_config_directory_and_rejects_invalid_credentials_privately() {
    let tree = TempTree::new();
    for (config_home, directory) in [
        ("relative-config", ".config/token-tracker"),
        ("config", "config/token-tracker"),
    ] {
        let config_home = if config_home == "config" {
            tree.root.join(config_home)
        } else {
            config_home.into()
        };
        let directory = tree.root.join(directory);
        tree.write(
            directory.join("config.toml"),
            "remote_url = 'https://remote.example'\n",
        );
        let auth_path = directory.join("auth.toml");

        for content in [
            None,
            Some(""),
            Some("remote_token = '  '"),
            Some("remote_token = 'SECRET_unclosed"),
            Some("SECRET_unknown = 'SECRET_token'"),
            Some("remote_token = ['SECRET_token']"),
        ] {
            if let Some(content) = content {
                fs::write(&auth_path, content).unwrap();
            }
            for args in [vec!["sync"], vec!["--remote"]] {
                let output = command(&tree.root)
                    .env("XDG_CONFIG_HOME", &config_home)
                    .args(args)
                    .output()
                    .unwrap();
                assert!(!output.status.success());
                assert!(output.stdout.is_empty());
                let error = String::from_utf8_lossy(&output.stderr);
                assert!(error.contains(auth_path.to_str().unwrap()), "{error}");
                assert!(!error.contains("SECRET_"), "{error}");
            }
        }
        assert!(!tree.root.join(".local").exists());
        fs::remove_file(auth_path).unwrap();
    }

    successful_report(command(&tree.root).arg("summary").output().unwrap());
}
