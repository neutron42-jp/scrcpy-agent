//! Thin wrapper around the `adb` command line.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceState {
    Device,
    Unauthorized,
    Offline,
    NoPermissions,
    Other(String),
}

impl DeviceState {
    pub fn as_str(&self) -> &str {
        match self {
            DeviceState::Device => "device",
            DeviceState::Unauthorized => "unauthorized",
            DeviceState::Offline => "offline",
            DeviceState::NoPermissions => "no permissions",
            DeviceState::Other(other) => other,
        }
    }

    pub fn is_usable(&self) -> bool {
        matches!(self, DeviceState::Device)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceEntry {
    pub serial: String,
    pub state: DeviceState,
    pub model: Option<String>,
    pub product: Option<String>,
    pub transport: Option<String>,
}

impl DeviceEntry {
    /// Human readable summary for logs and the device picker.
    pub fn label(&self) -> String {
        match &self.model {
            Some(model) if self.state.is_usable() => format!("{model} ({})", self.serial),
            Some(model) => format!("{model} ({}) [{}]", self.serial, self.state.as_str()),
            None => format!("{} [{}]", self.serial, self.state.as_str()),
        }
    }
}

#[derive(Clone)]
pub struct Adb {
    path: String,
}

impl Default for Adb {
    fn default() -> Self {
        Self::new("adb")
    }
}

impl Adb {
    pub fn new(path: impl Into<String>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(&self.path);
        cmd.stdin(Stdio::null());
        cmd
    }

    pub fn version(&self) -> Result<String> {
        let out = self.run(&["version"], Duration::from_secs(10))?;
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if stdout.is_empty() {
            bail!("adb is not available at '{}'", self.path);
        }
        Ok(stdout)
    }

    pub fn devices(&self) -> Result<Vec<DeviceEntry>> {
        let out = self.run(&["devices", "-l"], Duration::from_secs(10))?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        Ok(parse_devices(&stdout))
    }

    pub fn device_state(&self, serial: &str) -> Result<DeviceState> {
        self.devices()?
            .into_iter()
            .find(|d| d.serial == serial)
            .map(|d| d.state)
            .ok_or_else(|| anyhow!("device {serial} is not attached"))
    }

    pub fn shell(&self, serial: &str, command: &str, timeout: Duration) -> Result<Output> {
        self.run(&["-s", serial, "shell", command], timeout)
    }

    pub fn shell_stdout(&self, serial: &str, command: &str, timeout: Duration) -> Result<String> {
        let out = self.shell(serial, command, timeout)?;
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    pub fn reverse(&self, serial: &str, abstract_name: &str, port: u16) -> Result<()> {
        let remote = format!("localabstract:{abstract_name}");
        let local = format!("tcp:{port}");
        let out = self.run(
            &["-s", serial, "reverse", &remote, &local],
            Duration::from_secs(10),
        )?;
        ensure_success(&out, "adb reverse")
    }

    pub fn reverse_remove(&self, serial: &str, abstract_name: &str) -> Result<()> {
        let remote = format!("localabstract:{abstract_name}");
        let out = self.run(
            &["-s", serial, "reverse", "--remove", &remote],
            Duration::from_secs(10),
        )?;
        ensure_success(&out, "adb reverse --remove")
    }

    pub fn forward(&self, serial: &str, abstract_name: &str, port: u16) -> Result<()> {
        let local = format!("tcp:{port}");
        let remote = format!("localabstract:{abstract_name}");
        let out = self.run(
            &["-s", serial, "forward", &local, &remote],
            Duration::from_secs(10),
        )?;
        ensure_success(&out, "adb forward")
    }

    pub fn forward_remove(&self, serial: &str, port: u16) -> Result<()> {
        let local = format!("tcp:{port}");
        let out = self.run(
            &["-s", serial, "forward", "--remove", &local],
            Duration::from_secs(10),
        )?;
        ensure_success(&out, "adb forward --remove")
    }

    pub fn push(&self, serial: &str, local: &Path, remote: &str) -> Result<()> {
        let local = local.to_string_lossy().to_string();
        let out = self.run(
            &["-s", serial, "push", &local, remote],
            Duration::from_secs(60),
        )?;
        ensure_success(&out, "adb push")
    }

    pub fn spawn_shell(&self, serial: &str, command: &str) -> Result<Child> {
        let child = self
            .command()
            .args(["-s", serial, "shell", command])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to spawn {} shell", self.path))?;
        Ok(child)
    }

    pub fn run(&self, args: &[&str], timeout: Duration) -> Result<Output> {
        let mut cmd = self.command();
        cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to run {} {}", self.path, args.join(" ")))?;

        let mut stdout_pipe = child.stdout.take().expect("stdout piped");
        let mut stderr_pipe = child.stderr.take().expect("stderr piped");
        let (tx_out, rx_out) = mpsc::channel();
        let (tx_err, rx_err) = mpsc::channel();
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = stdout_pipe.read_to_end(&mut buf);
            let _ = tx_out.send(buf);
        });
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = stderr_pipe.read_to_end(&mut buf);
            let _ = tx_err.send(buf);
        });

        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(err) => return Err(err.into()),
            }
            if started.elapsed() > timeout {
                let _ = child.kill();
                let _ = child.wait();
                bail!(
                    "{} {} timed out after {:?}",
                    self.path,
                    args.join(" "),
                    timeout
                );
            }
            thread::sleep(Duration::from_millis(10));
        };

        let stdout = rx_out.recv().unwrap_or_default();
        let stderr = rx_err.recv().unwrap_or_default();
        Ok(Output {
            status,
            stdout,
            stderr,
        })
    }
}

fn ensure_success(out: &Output, what: &str) -> Result<()> {
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    bail!(
        "{what} failed ({}): {}{}",
        out.status,
        stdout.trim(),
        stderr.trim()
    )
}

pub fn parse_devices(output: &str) -> Vec<DeviceEntry> {
    let mut devices = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty()
            || line.starts_with("List of devices")
            || line.starts_with('*')
            || line.starts_with("adb server")
        {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(serial) = parts.next() else { continue };
        let state_token = parts.next().unwrap_or("unknown");
        let state = match state_token {
            "device" => DeviceState::Device,
            "unauthorized" => DeviceState::Unauthorized,
            "offline" => DeviceState::Offline,
            "no" => DeviceState::NoPermissions,
            other => DeviceState::Other(other.to_string()),
        };
        let mut model = None;
        let mut product = None;
        let mut transport = None;
        for part in parts {
            if let Some(value) = part.strip_prefix("model:") {
                model = Some(value.replace('_', " "));
            } else if let Some(value) = part.strip_prefix("product:") {
                product = Some(value.to_string());
            } else if let Some(value) = part.strip_prefix("transport_id:") {
                transport = Some(value.to_string());
            }
        }
        devices.push(DeviceEntry {
            serial: serial.to_string(),
            state,
            model,
            product,
            transport,
        });
    }
    devices
}

/// Writes a single scrcpy control message. Used by session tests and the control writer.
pub fn write_all(stream: &mut impl Write, bytes: &[u8]) -> std::io::Result<()> {
    stream.write_all(bytes)?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_adb_devices_output() {
        let input = "List of devices attached\n\
                     R58M1234ABC          device product:beyond1q model:SM_G973F device:beyond1 transport_id:2\n\
                     emulator-5554        offline product:sdk_gphone64_x86_64 model:sdk_gphone64_x86_64 transport_id:1\n\
                     ZY22ABCDEF           unauthorized usb:1-1 transport_id:3\n";
        let devices = parse_devices(input);
        assert_eq!(devices.len(), 3);
        assert_eq!(devices[0].serial, "R58M1234ABC");
        assert_eq!(devices[0].model.as_deref(), Some("SM G973F"));
        assert!(devices[0].state.is_usable());
        assert_eq!(devices[1].state, DeviceState::Offline);
        assert_eq!(devices[2].state, DeviceState::Unauthorized);
    }

    #[test]
    fn ignores_daemon_noise() {
        let input = "* daemon not running; starting now at tcp:5037\n\
                     * daemon started successfully\n\
                     List of devices attached\n";
        assert!(parse_devices(input).is_empty());
    }
}
