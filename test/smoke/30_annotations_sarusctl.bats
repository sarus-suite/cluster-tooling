#!/usr/bin/env bats

load test_helper.bash
load /usr/local/lib/bats-support/load
load /usr/local/lib/bats-assert/load

setup_file() {
  local repo_root

  smoke_require_cmds_or_skip podman parallax parallax-mount-program mksquashfs
  repo_root="$(smoke_repo_root)"
  if [ ! -x "${repo_root}/dist/sarusctl" ]; then
    skip "missing required binary: dist/sarusctl"
  fi
  smoke_init_file_env
}

teardown_file() {
  smoke_cleanup_file_env
}

@test "edf annotations set a custom mount-program logfile for srun" {
  smoke_require_cmds_or_skip srun

  smoke_write_edf "busybox" "busybox:latest" "
[annotations]
com.sarus.parallax_mp_logfile = \"${ANNOTATION_LOGFILE}\"
com.sarus.parallax_mp_squashfuse_path = \"squashfuse_ll\"
"

  run srun -p debug -t 3 -A default -J logfile-test -n 1 --edf=busybox true
  assert_success

  run stat -c '%s %n' "$ANNOTATION_LOGFILE"
  assert_success
  assert_output --regexp '^[1-9][0-9]* .+annotation\.log$'
}

@test "sarusctl run with a busybox edf succeeds" {
  smoke_write_edf "busybox" "busybox:latest"

  run "$SARUSCTL_BINARY" run busybox echo "ok :D"
  assert_success
  assert_output --partial "ok :D"
}

@test "sarusctl run honors the mount-program logfile annotation" {
  smoke_write_edf "busybox" "busybox:latest" "
[annotations]
com.sarus.parallax_mp_logfile = \"${ANNOTATION_LOGFILE}\"
com.sarus.parallax_mp_squashfuse_path = \"squashfuse_ll\"
"

  run "$SARUSCTL_BINARY" --verbose run busybox true
  assert_success

  run stat -c '%s %n' "$ANNOTATION_LOGFILE"
  assert_success
  assert_output --regexp '^[1-9][0-9]* .+annotation\.log$'
}
