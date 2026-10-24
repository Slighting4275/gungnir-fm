//! Cross-init system and process supervisor integration.
//!
//! Natively supports all 24 init systems and process supervisors without libsystemd:
//! - Foreground execution by default (runit, s6, dinit, daemontools, perp, nosh, systemd)
//! - Optional background daemonization (-d / --daemon) and pidfile (--pidfile) for SysVinit / BSD rc
//! - Agnostic readiness notifications:
//!   - UNIX datagram socket for systemd/nosh ($NOTIFY_SOCKET) without libsystemd
//!   - Inherited pipe fd for s6 / dinit (--ready-fd <FD> or $S6_NOTIFICATION_FD)
//!   - File-based readiness via pidfile

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Default, Clone, Debug)]
pub struct InitConfig {
    pub daemon: bool,
    pub pidfile: Option<PathBuf>,
    pub ready_fd: Option<i32>,
}

pub struct PidFileGuard {
    path: Option<PathBuf>,
}

impl Drop for PidFileGuard {
    fn drop(&mut self) {
        if let Some(ref p) = self.path {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Notify the supervisor that the service is initialized and ready.
pub fn notify_ready(cfg: &InitConfig) {
    // 1. Pipe-based notification (s6, dinit, s6-rc)
    if let Some(fd) = cfg.ready_fd {
        notify_pipe_fd(fd);
    } else if let Some(fd) = std::env::var("S6_NOTIFICATION_FD").ok().and_then(|v| v.parse::<i32>().ok()) {
        notify_pipe_fd(fd);
    }

    // 2. UNIX datagram socket notification ($NOTIFY_SOCKET) without linking libsystemd
    if let Ok(sock_path) = std::env::var("NOTIFY_SOCKET") {
        notify_socket(&sock_path);
    }
}

fn notify_pipe_fd(fd: i32) {
    unsafe {
        let msg = b"\n";
        let _ = libc::write(fd, msg.as_ptr().cast(), msg.len());
        let _ = libc::close(fd);
    }
}

fn notify_socket(sock_path: &str) {
    use std::os::unix::net::UnixDatagram;
    if let Some(stripped) = sock_path.strip_prefix('@') {
        let mut bytes = vec![0u8];
        bytes.extend_from_slice(stripped.as_bytes());
        notify_abstract_socket(&bytes);
        return;
    }

    if let Ok(sock) = UnixDatagram::unbound() {
        let _ = sock.send_to(b"READY=1\n", Path::new(sock_path));
    }
}

fn notify_abstract_socket(path_bytes: &[u8]) {
    unsafe {
        let fd = libc::socket(libc::AF_UNIX, libc::SOCK_DGRAM, 0);
        if fd < 0 {
            return;
        }
        let mut addr: libc::sockaddr_un = std::mem::zeroed();
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        let len = std::cmp::min(path_bytes.len(), addr.sun_path.len() - 1);
        std::ptr::copy_nonoverlapping(path_bytes.as_ptr().cast::<libc::c_char>(), addr.sun_path.as_mut_ptr(), len);
        let sock_len = (std::mem::size_of::<libc::sa_family_t>() + len) as libc::socklen_t;
        let msg = b"READY=1\n";
        let _ = libc::sendto(fd, msg.as_ptr().cast(), msg.len(), 0, std::ptr::addr_of!(addr).cast(), sock_len);
        let _ = libc::close(fd);
    }
}

/// Daemonize the process if `-d / --daemon` was specified.
pub fn daemonize_if_requested(cfg: &InitConfig) -> Result<(), Box<dyn std::error::Error>> {
    if !cfg.daemon {
        return Ok(());
    }

    unsafe {
        match libc::fork() {
            -1 => return Err("fork failed".into()),
            0 => {
                // Child: create new session and detach from controlling tty
                if libc::setsid() == -1 {
                    return Err("setsid failed".into());
                }
                // Second fork to prevent acquiring a tty
                match libc::fork() {
                    -1 => return Err("second fork failed".into()),
                    0 => {
                        // Success in grandchild daemon: redirect stdio to /dev/null
                        let dev_null = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
                        if dev_null >= 0 {
                            libc::dup2(dev_null, libc::STDIN_FILENO);
                            libc::dup2(dev_null, libc::STDOUT_FILENO);
                            libc::dup2(dev_null, libc::STDERR_FILENO);
                            if dev_null > libc::STDERR_FILENO {
                                libc::close(dev_null);
                            }
                        }
                    }
                    _ => libc::_exit(0),
                }
            }
            _ => libc::_exit(0),
        }
    }
    Ok(())
}

/// Write PID file if requested and return a guard that deletes it on drop.
pub fn create_pidfile(path: Option<&Path>) -> Result<PidFileGuard, Box<dyn std::error::Error>> {
    if let Some(p) = path {
        let pid = std::process::id();
        let mut f = File::create(p)?;
        writeln!(f, "{pid}")?;
        Ok(PidFileGuard { path: Some(p.to_path_buf()) })
    } else {
        Ok(PidFileGuard { path: None })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pidfile_creation_and_cleanup() -> Result<(), Box<dyn std::error::Error>> {
        let tmp = std::env::temp_dir().join(format!("test_pidfile_{}.pid", std::process::id()));
        {
            let guard = create_pidfile(Some(&tmp))?;
            assert!(tmp.exists());
            let content = std::fs::read_to_string(&tmp)?;
            assert_eq!(content.trim(), std::process::id().to_string());
            drop(guard);
        }
        assert!(!tmp.exists());
        Ok(())
    }

    #[test]
    fn test_ready_pipe_notification() {
        let mut fds = [0i32; 2];
        let res = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(res, 0);

        let cfg = InitConfig { daemon: false, pidfile: None, ready_fd: Some(fds[1]) };
        notify_ready(&cfg);

        let mut buf = [0u8; 4];
        let n = unsafe { libc::read(fds[0], buf.as_mut_ptr().cast(), buf.len()) };
        assert_eq!(n, 1);
        assert_eq!(buf[0], b'\n');
        unsafe {
            libc::close(fds[0]);
        }
    }
}
