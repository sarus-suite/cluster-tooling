#[test]
fn executable_reports_unimplemented_commands_without_initializing() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("store");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_hpc-image-store"))
        .arg("--store")
        .arg(&root)
        .arg("list")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("not implemented yet")
    );
    assert!(!root.exists());
}
