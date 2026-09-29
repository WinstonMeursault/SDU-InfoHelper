use std::{fs, process::Command};

#[test]
fn config_defaults_to_the_runtime_working_directory() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("config.yaml"), "{}\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sdu-infohelper"))
        .current_dir(dir.path())
        .env_remove("SDU_INFOHELPER_CONFIG")
        .arg("check-config")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn config_can_be_set_with_an_environment_variable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("other.yaml");
    fs::write(&path, "{}\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sdu-infohelper"))
        .current_dir(dir.path())
        .env("SDU_INFOHELPER_CONFIG", path)
        .arg("check-config")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}
