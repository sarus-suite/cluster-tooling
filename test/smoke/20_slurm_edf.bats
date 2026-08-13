#!/usr/bin/env bats

load test_helper.bash
load /usr/local/lib/bats-support/load
load /usr/local/lib/bats-assert/load

setup_file() {
  smoke_require_cmds_or_skip podman parallax parallax-mount-program mksquashfs srun timeout
  smoke_init_file_env
}

teardown_file() {
  smoke_cleanup_file_env
}

@test "srun can run podman from the parallax ro store" {
  smoke_prepare_busybox_ro_store

  run env \
    PODMAN_BINARY="$PODMAN_BINARY" \
    PODMAN_RUNROOT="$PODMAN_RUNROOT" \
    CLEAN_ROOT="$CLEAN_ROOT" \
    RO_STORAGE="$RO_STORAGE" \
    MOUNT_PROGRAM_PATH="$MOUNT_PROGRAM_PATH" \
    srun -p debug -A default -J srun-podman-rostore -n 1 bash -lc '
      "$PODMAN_BINARY" \
        --root "$CLEAN_ROOT" \
        --runroot "$PODMAN_RUNROOT" \
        --storage-opt additionalimagestore="$RO_STORAGE" \
        --storage-opt mount_program="$MOUNT_PROGRAM_PATH" \
        run --rm busybox:latest echo "ok (from srun + parallax ro-store)"
    '
  assert_success
  assert_output "ok (from srun + parallax ro-store)"
}

@test "srun with a busybox edf succeeds" {
  smoke_write_edf "busybox" "busybox:latest"

  run srun -p debug -t 3 -A default -J srun-skybox --chdir=/tmp -n 1 --edf=busybox echo "ok"
  assert_success
  assert_output "ok"
}

@test "srun with an ubuntu edf can stat expected files" {
  smoke_write_edf "ubuntu" "library/ubuntu:22.04"

  run srun -p debug -t 3 -A default -J srun-skybox-2 -n 1 --edf=ubuntu \
    stat -c '%u %g %n' /etc/os-release /bin/sh
  assert_success
  assert_output --partial "/etc/os-release"
  assert_output --partial "/bin/sh"
}

@test "missing edf is reported once through the user-facing SPANK log" {
  local report_count

  run srun -p debug -t 3 -A default -J srun-skybox-missing-edf \
    -n 1 --edf=skybox-ci-missing-edf true

  assert_failure
  assert_output --partial "[skybox] Error 006"
  assert_output --partial "skybox-ci-missing-edf"

  report_count="$(
    awk 'index($0, "[skybox] Error 006") { count++ } END { print count + 0 }' <<<"$output"
  )"
  assert_equal "$report_count" "1"
}

@test "podman startup failure is detailed once and promptly propagated to sibling tasks" {
  local detail_count

  smoke_write_edf "broken-device" "busybox:latest" \
    'devices = ["/dev/skybox-ci-missing-device"]'

  run timeout 15s srun -p debug -t 3 -A default -J srun-skybox-start-failure \
    -N 1 -n 2 --ntasks-per-node=2 --edf=broken-device true

  assert_failure
  if [ "$status" -eq 124 ]; then
    echo "srun timed out instead of propagating the Podman startup failure" >&3
    false
  fi

  assert_output --partial "[skybox]"
  assert_output --partial "failed with exit status"
  assert_output --partial "/dev/skybox-ci-missing-device"
  assert_output --partial "Podman container startup failed on local task 0"

  detail_count="$(
    awk 'index($0, "failed with exit status") { count++ } END { print count + 0 }' <<<"$output"
  )"
  assert_equal "$detail_count" "1"
}

@test "podman remove busybox from the parallax ro store" {
  run "$PARALLAX_BINARY" \
    --podmanRoot "$PODMAN_ROOT" \
    --roStoragePath "$RO_STORAGE" \
    --log-level info \
    --rmi \
    --image busybox:latest
  assert_success

  run "$PODMAN_BINARY" --root "$PODMAN_ROOT" --runroot "$PODMAN_RUNROOT" rmi busybox:latest
  assert_success

  rm "${RO_STORAGE}/.busybox-ready"
}

@test "podman remove ubuntu from the parallax ro store" {
  run "$PARALLAX_BINARY" \
    --podmanRoot "$PODMAN_ROOT" \
    --roStoragePath "$RO_STORAGE" \
    --log-level info \
    --rmi \
    --image library/ubuntu:22.04
  assert_success
}
