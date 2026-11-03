use std::fs;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use scrcpy_agent::android::session::{log_sink, SessionOptions};
use scrcpy_agent::android::{Adb, Session};

fn write_fake_adb(dir: &Path) -> std::path::PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let adb_path = dir.join("adb");
    let script = format!(
        r#"#!/usr/bin/env bash
set -u
echo "$*" >> "{log}"
args="$*"
case "$args" in
  *"devices -l"*)
      printf 'List of devices attached\nFAKE123\tdevice product:fake model:FakePhone transport_id:1\n'
      exit 0 ;;
  *"reverse localabstract:"*" tcp:"*)
      echo "$args" | sed 's/.*tcp://' > "{port}"
      exit 0 ;;
  *"reverse --remove"*) exit 0 ;;
  *"forward tcp:"*) exit 0 ;;
  *"push "*) exit 0 ;;
  *"shell stat -c %s"*) echo 1; exit 0 ;;
  *"shell CLASSPATH="*)
      port=$(cat "{port}")
      python3 "{server}" "$port" "{h264}" "{control}" &
      child=$!
      trap 'kill "$child" 2>/dev/null' TERM INT EXIT
      wait "$child"
      exit 0 ;;
  *) exit 0 ;;
esac
"#,
        log = dir.join("adb.log").display(),
        port = dir.join("port").display(),
        server = root.join("tests/fixtures/fake_server.py").display(),
        h264 = root.join("tests/fixtures/sample.h264").display(),
        control = dir.join("control.bin").display(),
    );
    fs::write(&adb_path, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&adb_path, fs::Permissions::from_mode(0o755)).unwrap();
    adb_path
}

#[test]
fn start_runs_adb_end_to_end_against_a_fake_server() {
    let dir = tempfile::tempdir().unwrap();
    let adb_path = write_fake_adb(dir.path());
    let adb = Adb::new(adb_path.to_string_lossy());

    let (tx, rx) = mpsc::channel();
    let adb_for_thread = adb.clone();
    thread::spawn(move || {
        let result = Session::start(
            "FAKE123".to_string(),
            &adb_for_thread,
            SessionOptions::default(),
            log_sink(|level, message| eprintln!("[{level:?}] {message}")),
        );
        let _ = tx.send(result);
    });

    let session = rx
        .recv_timeout(Duration::from_secs(20))
        .expect("Session::start timed out")
        .expect("Session::start failed");

    let deadline = Instant::now() + Duration::from_secs(5);
    while session.frames().latest().is_none() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    let frame = session.frames().latest().expect("decoded frame");
    assert_eq!((frame.width, frame.height), (160, 120));
    assert_eq!(session.status().device_name, "FakePhone");

    session.tap(11, 22, 50).expect("tap");

    let control_path = dir.path().join("control.bin");
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if fs::metadata(&control_path)
            .map(|meta| meta.len() >= 64)
            .unwrap_or(false)
        {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let control = fs::read(&control_path).expect("control log");
    assert!(
        control.len() >= 64,
        "expected touch events, got {} bytes",
        control.len()
    );
    assert_eq!(&control[..2], &[2, 0]);
    assert_eq!(&control[32..34], &[2, 1]);

    session.stop();

    let adb_log = fs::read_to_string(dir.path().join("adb.log")).unwrap();
    assert!(adb_log.contains("reverse --remove"), "adb log: {adb_log}");
    assert!(
        adb_log.contains("push "),
        "server jar was not pushed: {adb_log}"
    );
}
