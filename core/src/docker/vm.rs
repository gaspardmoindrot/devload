//! Which process runs the Docker engine's Linux VM, so containers' memory is
//! counted once: it is already in that process's footprint, and moves from
//! it to the stacks.
//!
//! Every Docker setup on macOS runs Linux in a VM process:
//! - Apple Virtualization: `com.apple.Virtualization.VirtualMachine`, an XPC
//!   service macOS holds its launcher responsible for. Docker Desktop,
//!   OrbStack, and Colima, Lima, Rancher Desktop with `vmType: vz`.
//! - QEMU (`qemu-system-*`): Colima, Lima, Rancher Desktop, Podman.
//! - Docker Desktop's Docker VMM (`com.docker.krun`), Podman's `vfkit` and
//!   `krunkit`, OrbStack's `OrbStack Helper`.
//!
//! The provider is read from the socket path, and each VM process is tied to
//! a provider by its app, or by the paths in its (or its launcher's) command
//! line. Other VMs (UTM, Tart, a second engine) keep their memory.

use std::path::Path;

/// Who runs the Docker engine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Provider {
    DockerDesktop,
    OrbStack,
    Colima,
    RancherDesktop,
    Lima,
    Podman,
    /// Not recognised (or no socket).
    #[default]
    Unknown,
}

impl Provider {
    /// From the socket the engine is read through. A symlink
    /// (`/var/run/docker.sock` → `~/.docker/run/docker.sock` or
    /// `~/.orbstack/run/docker.sock`) is followed.
    pub fn of_socket(path: &Path) -> Provider {
        let p = Provider::of_text(&path.to_string_lossy());
        if p != Provider::Unknown {
            return p;
        }
        match std::fs::canonicalize(path) {
            Ok(real) if real != path => Provider::of_text(&real.to_string_lossy()),
            _ => Provider::Unknown,
        }
    }

    /// From a path or command line that names its provider. Order matters:
    /// Colima and Rancher Desktop run Lima, whose paths they contain.
    pub fn of_text(text: &str) -> Provider {
        let t = text.to_ascii_lowercase();
        let has = |s: &str| t.contains(s);
        if has("orbstack") {
            Provider::OrbStack
        } else if has("colima") {
            Provider::Colima
        } else if has("rancher-desktop") || has("rancher desktop") || has("/.rd/") {
            Provider::RancherDesktop
        } else if has("podman") {
            Provider::Podman
        } else if has("/.lima/") || has("/lima/") || has("limactl") {
            Provider::Lima
        } else if has("/docker.app") || has("/.docker/") || has("com.docker.") {
            Provider::DockerDesktop
        } else {
            Provider::Unknown
        }
    }
}

/// Whether a process (kernel name, executable) hosts a VM's memory.
pub fn is_vm_process(name: &str, exe: Option<&str>) -> bool {
    let base = exe.and_then(|e| e.rsplit('/').next()).unwrap_or("");
    name.starts_with("com.apple.Virtu")
        || name.starts_with("qemu-system")
        || base.starts_with("qemu-system")
        || matches!(name, "com.docker.krun" | "vfkit" | "krunkit" | "OrbStack Helper")
}

/// The VM processes whose memory holds the containers of `provider`, as
/// indices into `vms` (`(provider of each VM process, Unknown when it can't
/// be told)`). With an Unknown provider, the one provider all attributed VMs
/// share, if any. When no VM is tied to it, the VMs that could not be tied
/// to any provider (a Colima VM launched from a terminal is held by the
/// terminal app).
pub fn engine_vms(provider: Provider, vms: &[Provider]) -> Vec<usize> {
    let mut known = vms.iter().copied().filter(|&p| p != Provider::Unknown);
    let chosen = match provider {
        Provider::Unknown => match known.next() {
            Some(first) if known.all(|p| p == first) => first,
            _ => Provider::Unknown,
        },
        p => p,
    };
    let pick = |want: Provider| (0..vms.len()).filter(|&i| vms[i] == want).collect::<Vec<_>>();
    if chosen != Provider::Unknown {
        let found = pick(chosen);
        if !found.is_empty() {
            return found;
        }
    }
    // Unattributed VMs only stand in for a known provider, or when no VM is
    // attributed at all.
    if chosen != Provider::Unknown || vms.iter().all(|&p| p == Provider::Unknown) { pick(Provider::Unknown) } else { Vec::new() }
}

#[cfg(test)]
mod tests {
    use super::Provider::*;
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn provider_of_each_socket() {
        let cases = [
            ("/Users/dev/.docker/run/docker.sock", DockerDesktop),
            ("/Users/dev/.orbstack/run/docker.sock", OrbStack),
            ("/Users/dev/.colima/default/docker.sock", Colima),
            ("/Users/dev/.colima/work/docker.sock", Colima),
            ("/Users/dev/.rd/docker.sock", RancherDesktop),
            ("/Users/dev/.lima/docker/sock/docker.sock", Lima),
            ("/var/folders/xy/T/podman/podman-machine-default-api.sock", Podman),
            ("/Users/dev/.local/share/containers/podman/machine/podman.sock", Podman),
            ("/tmp/devload-no-such-dir/docker.sock", Unknown),
        ];
        for (path, want) in cases {
            assert_eq!(Provider::of_socket(Path::new(path)), want, "{path}");
        }
    }

    #[test]
    fn var_run_symlink_is_followed() {
        let dir = std::env::temp_dir().join(format!("devload-vm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let target = dir.join(".orbstack/run");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("docker.sock"), "").unwrap();
        let link: PathBuf = dir.join("docker.sock");
        std::os::unix::fs::symlink(target.join("docker.sock"), &link).unwrap();
        assert_eq!(Provider::of_socket(&link), OrbStack);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn provider_of_vm_command_lines() {
        let cases = [
            ("/Applications/Docker.app", DockerDesktop),
            ("/Applications/OrbStack.app", OrbStack),
            ("/Applications/Rancher Desktop.app", RancherDesktop),
            ("/Applications/Podman Desktop.app", Podman),
            ("/Applications/UTM.app", Unknown),
            ("/System/Applications/Utilities/Terminal.app", Unknown),
            // Colima's hostagent, and its QEMU (paths under ~/.colima/_lima).
            ("limactl hostagent --pidfile /Users/dev/.colima/_lima/colima/ha.pid colima", Colima),
            ("qemu-system-aarch64 -name lima-colima -drive file=/Users/dev/.colima/_lima/colima/diffdisk", Colima),
            ("limactl hostagent --pidfile /Users/dev/Library/Application Support/rancher-desktop/lima/0/ha.pid 0", RancherDesktop),
            ("limactl hostagent --pidfile /Users/dev/.lima/docker/ha.pid docker", Lima),
            ("vfkit --cpus 4 --bootloader efi,variable-store=/Users/dev/.local/share/containers/podman/machine/applehv/efi", Podman),
        ];
        for (text, want) in cases {
            assert_eq!(Provider::of_text(text), want, "{text}");
        }
    }

    #[test]
    fn vm_processes() {
        assert!(is_vm_process("com.apple.Virtualization.Virtua", None));
        assert!(is_vm_process("qemu-system-aarc", Some("/opt/homebrew/bin/qemu-system-aarch64")));
        assert!(is_vm_process("qemu-system-x86_", None));
        assert!(is_vm_process("OrbStack Helper", None));
        assert!(is_vm_process("com.docker.krun", None));
        assert!(is_vm_process("vfkit", None));
        assert!(!is_vm_process("limactl", Some("/opt/homebrew/bin/limactl")));
        assert!(!is_vm_process("com.docker.backend", None));
    }

    #[test]
    fn engine_vms_picks_the_socket_provider() {
        // Docker Desktop and OrbStack both running, socket on OrbStack.
        assert_eq!(engine_vms(OrbStack, &[DockerDesktop, OrbStack, OrbStack]), [1, 2]);
        // Socket unknown: the one provider seen.
        assert_eq!(engine_vms(Unknown, &[Colima]), [0]);
        // Socket unknown and two providers: can't tell, none.
        assert_eq!(engine_vms(Unknown, &[DockerDesktop, OrbStack]), Vec::<usize>::new());
        // Colima's VM held by the terminal: the unattributed VM stands in,
        // never Docker Desktop's.
        assert_eq!(engine_vms(Colima, &[DockerDesktop, Unknown]), [1]);
        // A UTM VM next to Docker Desktop's is left alone.
        assert_eq!(engine_vms(DockerDesktop, &[Unknown, DockerDesktop]), [1]);
        assert_eq!(engine_vms(Unknown, &[Unknown, DockerDesktop]), [1]);
        // Only unattributed VMs and an unknown socket: they're all we have.
        assert_eq!(engine_vms(Unknown, &[Unknown]), [0]);
        assert_eq!(engine_vms(Colima, &[]), Vec::<usize>::new());
    }
}
