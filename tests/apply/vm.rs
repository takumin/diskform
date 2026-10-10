//! Booting a VM per test on the host (ADR 0017).

use std::env;
use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::{MIB, Test};

const FILES: [&str; 4] = ["cloud-hypervisor", "vmlinuz", "initrd", "rootfs.qcow2"];
const TIMEOUT: Duration = Duration::from_secs(600);
/// The line with which the guest reports the exit status of a test.
const EXIT: &str = "diskform-apply-exit: ";

/// The root file system is read only, and the guest writes to a tmpfs over
/// it. The guest mounts the payload disk, runs `/run/payload/run` in place
/// of the usual services, and powers off. `fstab=no` keeps the guest from
/// mounting the other partitions of the image.
const CMDLINE: &str = "console=ttyS0 root=LABEL=cloudimg-rootfs ro rd.overlay fstab=no \
                       cloud-init=disabled \
                       systemd.mount-extra=/dev/disk/by-id/virtio-payload:/run/payload:ext4:ro \
                       systemd.run=/run/payload/run systemd.run_success_action=poweroff \
                       systemd.run_failure_action=poweroff";

pub struct Vm {
    dir: PathBuf,
}

impl Vm {
    pub fn new(dir: PathBuf) -> Result<Self, String> {
        for file in FILES {
            if !dir.join(file).is_file() {
                return Err(format!(
                    "{} has no {file}; prepare it with tests/apply/fetch-vm.sh",
                    dir.display()
                ));
            }
        }
        OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/kvm")
            .map_err(|e| format!("cannot open /dev/kvm, which the VM needs: {e}"))?;
        Ok(Vm { dir })
    }

    /// Boots a VM that runs `test`, and returns its output if it fails.
    pub fn run(&self, test: &Test) -> Result<(), String> {
        let work = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join("apply")
            .join(test.name);
        if work.exists() {
            fs::remove_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
        }
        let payload = payload(&work, test.name)?;
        let mut disks = vec![
            format!(
                "path={},readonly=on,image_type=qcow2",
                self.dir.join("rootfs.qcow2").display()
            ),
            format!("path={},readonly=on,serial=payload", payload.display()),
        ];
        for (serial, size) in test.disks {
            let image = work.join(format!("{serial}.img"));
            File::create(&image)
                .and_then(|f| f.set_len(*size))
                .map_err(|e| format!("{}: {e}", image.display()))?;
            disks.push(format!("path={},serial={serial}", image.display()));
        }

        let console = work.join("console.log");
        let serial = work.join("serial.log");
        let log = File::create(work.join("cloud-hypervisor.log")).map_err(|e| e.to_string())?;
        let mut child = Command::new(self.dir.join("cloud-hypervisor"))
            .arg("--kernel")
            .arg(self.dir.join("vmlinuz"))
            .arg("--initramfs")
            .arg(self.dir.join("initrd"))
            .args(["--cmdline", CMDLINE])
            .args(["--cpus", "boot=2", "--memory", "size=2048M"])
            .arg("--disk")
            .args(&disks)
            .arg("--serial")
            .arg(format!("file={}", serial.display()))
            .arg("--console")
            .arg(format!("file={}", console.display()))
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(|e| e.to_string())?)
            .stderr(log)
            .spawn()
            .map_err(|e| format!("cannot run cloud-hypervisor: {e}"))?;

        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                break Some(status);
            }
            if started.elapsed() > TIMEOUT {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            sleep(Duration::from_millis(200));
        };

        let output = read(&console);
        let exit = output
            .lines()
            .find_map(|l| l.strip_prefix(EXIT))
            .map(str::trim);
        if exit == Some("0") {
            return Ok(());
        }
        let reason = match (status, exit) {
            (None, _) => format!("the VM did not power off within {TIMEOUT:?}"),
            (Some(_), Some(code)) => format!("the test exited with {code}"),
            (Some(status), None) => format!("the VM stopped ({status}) before the test ended"),
        };
        Err(format!(
            "{reason}\n--- test output ---\n{output}--- end of the guest's serial console ---\n{}\
             --- cloud-hypervisor ---\n{}(all logs are in {})",
            tail(&serial, 60),
            tail(&work.join("cloud-hypervisor.log"), 20),
            work.display()
        ))
    }
}

/// Makes the disk that carries the test and diskform into the guest.
fn payload(work: &Path, name: &str) -> Result<PathBuf, String> {
    let dir = work.join("payload");
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let exe = env::current_exe().map_err(|e| e.to_string())?;
    let copy = |from: &Path, to: &str| {
        fs::copy(from, dir.join(to)).map_err(|e| format!("{}: {e}", from.display()))
    };
    let mut size = copy(&exe, "apply")?;
    size += copy(Path::new(env!("CARGO_BIN_EXE_diskform")), "diskform")?;

    let run = dir.join("run");
    fs::write(
        &run,
        format!(
            "#!/bin/sh\n/run/payload/apply --guest {name} >/dev/hvc0 2>&1\n\
             echo \"{EXIT}$?\" >/dev/hvc0\n"
        ),
    )
    .and_then(|()| fs::set_permissions(&run, fs::Permissions::from_mode(0o755)))
    .map_err(|e| format!("{}: {e}", run.display()))?;

    let image = work.join("payload.img");
    File::create(&image)
        .and_then(|f| f.set_len(size + 64 * MIB))
        .map_err(|e| format!("{}: {e}", image.display()))?;
    let out = Command::new("mkfs.ext4")
        .args(["-q", "-F", "-d"])
        .arg(&dir)
        .arg(&image)
        .output()
        .map_err(|e| format!("cannot run mkfs.ext4: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "mkfs.ext4 failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(image)
}

fn tail(path: &Path, lines: usize) -> String {
    let text = read(path);
    let all: Vec<&str> = text.lines().collect();
    let mut out = all[all.len().saturating_sub(lines)..].join("\n");
    out.push('\n');
    out
}

/// Reads a log, which may hold bytes that are not UTF-8.
fn read(path: &Path) -> String {
    String::from_utf8_lossy(&fs::read(path).unwrap_or_default()).into_owned()
}
