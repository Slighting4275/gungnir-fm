# Native Cross-Init System and Process Supervisor Integration

Gungnir FM is architected to run natively across Linux and BSD distributions without any dependency on systemd, libsystemd, or systemd-specific D-Bus interfaces.

## Supported Init Systems & Process Supervisors (All 24 Native)

| Init / Supervisor | Configuration Location | Readiness Mechanism | Execution Model |
| :--- | :--- | :--- | :--- |
| **systemd** | `init/systemd/gungnir-fm.service`<br>`init/systemd/gungnird.service`<br>`init/systemd/gungnird.socket` | Datagram `$NOTIFY_SOCKET` / Socket activation | Foreground (`--foreground`) / Daemon (`gungnird`) |
| **OpenRC** | `init/openrc/gungnir-fm`<br>`init/openrc/gungnird` | Process check / `/run/*.pid` | Supervised background / foreground |
| **runit** | `init/runit/gungnir-fm/run`<br>`init/runit/gungnird/run` | Process tracking via PID | Foreground supervised (`--foreground`) |
| **s6 & s6-rc** | `init/s6/gungnir-fm/`<br>`init/s6/gungnird/run` | Pipe fd notification (`--ready-fd 3`) | Foreground supervised |
| **dinit** | `init/dinit/gungnir-fm`<br>`init/dinit/gungnird` | Pipe fd notification (`pipefd:3`) | Foreground supervised |
| **SysVinit / inittab**| `init/sysvinit/gungnir-fm` | PID file (`--pidfile /run/gungnir-fm.pid`) | `start-stop-daemon` |
| **BSD Init (rc.subr)**| `init/bsd/gungnir_fm`<br>`init/bsd/gungnird` | PID file (`--pidfile /var/run/*.pid`)| Native `rc.subr` |
| **GNU Shepherd** | `init/shepherd/gungnir-fm.scm` | Fork-exec supervisor | Foreground supervised |
| **BusyBox init** | `init/busybox/rc.gungnir-fm` | PID file / inittab respawn | Foreground / start-stop-daemon |
| **Toybox init** | `init/busybox/rc.gungnir-fm` | PID file / inittab respawn | Foreground / start-stop-daemon |
| **Finit** | `init/finit/gungnir-fm.conf` | Native process monitor | Foreground respawn |
| **procd (OpenWrt)** | `init/procd/gungnir-fm.init` | Native ubus process instance | Foreground respawn |
| **sinit** | Inittab / rc script | Process tracking | Foreground |
| **nosh** | `init/daemontools/gungnir-fm/run` | Datagram `$NOTIFY_SOCKET` / fd | Foreground |
| **daemontools** | `init/daemontools/gungnir-fm/run` | Process tracking via supervise | Foreground |
| **daemontools-encore**| `init/daemontools/gungnir-fm/run`| Process tracking | Foreground |
| **perp** | `init/daemontools/gungnir-fm/run` | perp supervise monitor | Foreground |
| **anopa** | `init/s6/gungnir-fm/` | s6/anopa longrun | Foreground |
| **cinit** | Service config | In-core process monitor | Foreground |
| **initng** | Service config | Process monitor | Foreground |
| **Upstart** | `init/upstart/gungnir-fm.conf` | In-core respawn | Foreground |
| **OpenLaunchD** | `init/launchd/org.gungnir.fm.plist`| Launchd daemon job | Foreground |
| **Epoch** | Service config | Epoch process supervisor | Foreground |
| **minit** | `init/daemontools/gungnir-fm/run` | In-core process monitor | Foreground |
| **simpleinit-msb** | Inittab entry | Process tracking | Foreground |

## Core Architectural Guarantees

1. **Foreground by Default**:
   `gungnir-fm` runs in the foreground by default (`-f / --foreground`), writing line-buffered log output directly to `stdout` and `stderr`. Process supervisors capture logs natively with tools like `svlogd`, `s6-log`, or syslog.
2. **Agnostic Readiness Notification**:
   - For `s6` and `dinit`, pass `--ready-fd <FD>` or set `$S6_NOTIFICATION_FD`. When initialized, `gungnir-fm` writes a newline byte (`\n`) to the file descriptor and closes it.
   - For `systemd` and `nosh`, if `$NOTIFY_SOCKET` is in the environment, `gungnir-fm` sends `READY=1\n` directly over standard POSIX UNIX domain datagram sockets without linking against `libsystemd`.
3. **Optional Daemonization for Legacy Inits**:
   - Legacy SysVinit and classic BSD rc scripts can invoke `gungnir-fm -d` or `--daemon --pidfile <PATH>` to double-fork, detach from the controlling terminal, and maintain a PID file with automatic cleanup on exit.
4. **POSIX /bin/sh Scripts**:
   All build and installation scripts are written strictly in portable POSIX `/bin/sh` syntax without bashisms.
