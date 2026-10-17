//! `dbconnector serve` must not write a banner or logs to stdout before a client speaks.

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

#[test]
fn serve_does_not_write_stdout_before_input() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_dbconnector"))
        .args(["serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn dbconnector");
    let mut stdout = child.stdout.take().expect("stdout");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = [0u8; 128];
        let read = stdout.read(&mut buf).map(|n| (n, buf));
        let _ = tx.send(read);
    });
    match rx.recv_timeout(Duration::from_millis(500)) {
        Err(mpsc::RecvTimeoutError::Timeout) => {}
        Ok(Ok((0, _))) => panic!("stdout closed before any MCP input"),
        Ok(Ok((n, buf))) => panic!(
            "stdout leaked {} bytes: {:?}",
            n,
            String::from_utf8_lossy(&buf[..n])
        ),
        Ok(Err(err)) => panic!("stdout read failed: {err}"),
        Err(mpsc::RecvTimeoutError::Disconnected) => panic!("stdout reader disconnected"),
    }
    let _ = child.kill();
    let _ = child.wait();
}
