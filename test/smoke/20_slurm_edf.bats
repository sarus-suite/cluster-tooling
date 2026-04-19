#!/usr/bin/env bats

load test_helper.bash
load /usr/local/lib/bats-support/load
load /usr/local/lib/bats-assert/load

setup_file() {
  smoke_require_cmds_or_skip podman parallax parallax-mount-program mksquashfs srun
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
    --image ubuntu:22.04
  assert_success

  run "$PODMAN_BINARY" --root "$PODMAN_ROOT" --runroot "$PODMAN_RUNROOT" rmi ubuntu:22.04
  assert_success
}

