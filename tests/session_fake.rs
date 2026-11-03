use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use scrcpy_agent::android::session::{attach, handshake_reverse, log_sink, SessionOptions};
use scrcpy_agent::android::LogLevel;

fn read_control(mut stream: TcpStream, tx: mpsc::Sender<Vec<u8>>) {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
    let mut buf = [0u8; 256];
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(3) {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(err) if err.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => break,
        }
    }
}

#[test]
fn handshake_streams_frames_and_injects_taps() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (control_tx, control_rx) = mpsc::channel::<Vec<u8>>();
    let fixture = std::fs::read("tests/fixtures/sample.h264").unwrap();

    let fake = thread::spawn(move || {
        let mut video = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let control = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let mut name = [0u8; 64];
        name[..6].copy_from_slice(b"fake-1");
        video.write_all(&name).unwrap();
        video.write_all(&0x6832_3634u32.to_be_bytes()).unwrap();

        let mut session_header = [0u8; 12];
        session_header[0] = 0x80;
        session_header[4..8].copy_from_slice(&160u32.to_be_bytes());
        session_header[8..12].copy_from_slice(&120u32.to_be_bytes());
        video.write_all(&session_header).unwrap();

        let mut media_header = [0u8; 12];
        let flags = (1u64 << 61) | 7;
        media_header[0..8].copy_from_slice(&flags.to_be_bytes());
        media_header[8..12].copy_from_slice(&(fixture.len() as u32).to_be_bytes());
        video.write_all(&media_header).unwrap();
        video.write_all(&fixture).unwrap();
        video.flush().unwrap();

        read_control(control, control_tx);
    });

    let log = log_sink(|level, message| {
        if matches!(level, LogLevel::Warn | LogLevel::Error) {
            eprintln!("[{level:?}] {message}");
        }
    });

    let handshake = handshake_reverse(&listener, Duration::from_secs(5)).expect("handshake");
    assert_eq!(handshake.device_name, "fake-1");
    assert_eq!(handshake.codec_id, 0x6832_3634);

    let session = attach(
        "fake-serial".to_string(),
        handshake.device_name,
        handshake.video,
        handshake.control,
        SessionOptions::default(),
        log,
        None,
        Vec::new(),
    )
    .expect("attach");

    let deadline = Instant::now() + Duration::from_secs(5);
    while session.dims().is_none() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(session.dims().unwrap().width, 160);

    let deadline = Instant::now() + Duration::from_secs(5);
    while session.frames().latest().is_none() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    let frame = session.frames().latest().expect("decoded frame");
    assert_eq!((frame.width, frame.height), (160, 120));
    assert_eq!(frame.rgba.len(), 160 * 120 * 4);

    let screenshot = session.screenshot().expect("screenshot");
    assert!(screenshot.png.starts_with(&[0x89, b'P', b'N', b'G']));

    session.tap(10, 20, 50).expect("tap");

    let mut received = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(3);
    while received.len() < 64 && Instant::now() < deadline {
        if let Ok(chunk) = control_rx.recv_timeout(Duration::from_millis(100)) {
            received.extend_from_slice(&chunk);
        }
    }
    assert!(
        received.len() >= 64,
        "expected two 32-byte touch messages, got {} bytes",
        received.len()
    );
    assert_eq!(received[0], 2, "touch message type");
    assert_eq!(received[1], 0, "ACTION_DOWN");
    assert_eq!(u64::from_be_bytes(received[2..10].try_into().unwrap()), 1);
    assert_eq!(u32::from_be_bytes(received[10..14].try_into().unwrap()), 10);
    assert_eq!(u32::from_be_bytes(received[14..18].try_into().unwrap()), 20);
    assert_eq!(
        u16::from_be_bytes(received[18..20].try_into().unwrap()),
        160
    );
    assert_eq!(
        u16::from_be_bytes(received[20..22].try_into().unwrap()),
        120
    );
    assert_eq!(received[32], 2, "second touch message type");
    assert_eq!(received[33], 1, "ACTION_UP");

    session.stop();
    let _ = fake.join();
}
