use bstr::ByteSlice;
use raster;
use sarus_suite_podman_driver::{self as pmd, ContainerCtx};
use std::fs::File;
use std::io::prelude::*;
use std::path::PathBuf;
use std::time::Instant;

#[test]
fn test_run_output() {
    let out = pmd::run_output(["--rm", "ubuntu:24.04", "cat", "/etc/os-release"], None);
    assert!(
        out.stdout
            .as_slice()
            .contains_str("PRETTY_NAME=\"Ubuntu 24.04 LTS\"")
    );
}

#[test]
fn test_run_from_edf_output() {
    let ctx = ContainerCtx {
        name: String::from("sarus_edf_test"),
        interactive: false,
        detach: false,
        set_env: true,
        pidfile: None,
    };

    let edf_path = std::env::current_dir()
        .unwrap()
        .join("tests/edf/alpine.toml");
    let edf =
        raster::render(edf_path.to_string_lossy().into_owned()).expect("Failed to render EDF");
    let out = pmd::run_from_edf_output(&edf, None, &ctx, ["grep", "PRETTY", "/etc/os-release"]);
    assert!(
        out.stdout
            .as_slice()
            .contains_str("PRETTY_NAME=\"Alpine Linux v3.22\"")
    );
}

#[test]
fn test_run_from_edf_detached_output() -> anyhow::Result<()> {
    let ctx = ContainerCtx {
        name: String::from("sarus_edf_test"),
        interactive: false,
        detach: true,
        set_env: true,
        pidfile: Some(PathBuf::from("/tmp/sarus-edf-test-pidfile")),
    };

    let edf_path = std::env::current_dir()
        .unwrap()
        .join("tests/edf/alpine.toml");
    let edf =
        raster::render(edf_path.to_string_lossy().into_owned()).expect("Failed rendering EDF");
    let out = pmd::run_from_edf_output(&edf, None, &ctx, ["sleep", "3"]);

    let run_stdout = str::from_utf8(&out.stdout)?;
    let run_stdout = String::from(run_stdout.trim());

    let insp_out = pmd::inspect(&ctx.name, Some("{{.Id}}"), None);
    let cnt_id = str::from_utf8(&insp_out.stdout)?;
    let cnt_id = cnt_id.trim();
    assert_eq!(run_stdout, cnt_id);
    Ok(())
}

// Pull and rmi tests are prone to fail image existence assertions (depending on concurrency)
// and cause repeated registry pulls.
// Consider removal.
#[test]
fn test_pull() {
    let image = "alpine:3.22";
    if pmd::image_exists(image, None) {
        pmd::rmi(image, None);
    }
    assert!(!pmd::image_exists(image, None));
    pmd::pull(image, None);
    assert!(pmd::image_exists(image, None));
}

#[test]
fn test_rmi() {
    let image = "alpine:3.22";
    if !pmd::image_exists(image, None) {
        pmd::pull(image, None);
    }
    assert!(pmd::image_exists(image, None));
    pmd::rmi(image, None);
    assert!(!pmd::image_exists(image, None));
}

#[test]
fn test_get_container_pid() -> anyhow::Result<()> {
    let cnt_name = String::from("sarus_get_cnt_pid_test");
    let run = pmd::run_output(
        [
            "--rm",
            "--detach",
            "--name",
            &cnt_name,
            "alpine:3.22",
            "sleep",
            "5",
        ],
        None,
    );
    assert!(run.status.success(), "Could not run container!");

    let t0 = Instant::now();
    let cnt_id = str::from_utf8(&run.stdout)?;
    let cnt_id = String::from(cnt_id.trim());

    let runroot = PathBuf::from("/run/user/1000/containers");
    let file_pid = pmd::get_container_pid_from_default_file(&cnt_id, Some(&runroot))?;
    let tend = t0.elapsed();
    println!("pid from file took: {:.3} ms", tend.as_secs_f64() * 1_000.0);

    let t0 = Instant::now();
    let inspect_pid = pmd::get_container_pid(&cnt_name, None)?;
    let tend = t0.elapsed();
    println!(
        "pid from inspect took: {:.3} ms",
        tend.as_secs_f64() * 1_000.0
    );
    assert_eq!(file_pid, inspect_pid);
    Ok(())
}

#[test]
fn test_get_container_pid_from_pidfile() -> anyhow::Result<()> {
    let ctx = ContainerCtx {
        name: String::from("sarus_read_cnt_pidfile_test"),
        interactive: false,
        detach: true,
        set_env: true,
        pidfile: Some(PathBuf::from("/tmp/sarus-edf-test-pidfile")),
    };

    let edf_path = std::env::current_dir()
        .unwrap()
        .join("tests/edf/alpine.toml");
    let edf =
        raster::render(edf_path.to_string_lossy().into_owned()).expect("Failed rendering EDF");
    let run = pmd::run_from_edf_output(&edf, None, &ctx, ["sleep", "5"]);
    assert!(run.status.success(), "Could not run container!");

    let mut cnt_pidfile = File::open(ctx.pidfile.as_ref().unwrap())?;
    let mut file_pid = String::new();
    cnt_pidfile.read_to_string(&mut file_pid)?;
    let file_pid: u32 = file_pid.parse()?;

    let inspect_pid = pmd::get_container_pid(&ctx.name, None)?;
    assert_eq!(file_pid, inspect_pid);

    std::fs::remove_file(ctx.pidfile.as_ref().unwrap())?;
    assert!(!std::fs::exists(ctx.pidfile.unwrap())?);
    Ok(())
}
