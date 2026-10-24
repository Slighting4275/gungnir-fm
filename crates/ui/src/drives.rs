//! Removable drives through udisks2 on the system D-Bus (what Nemo gets from gvfs).
//!
//! `GetManagedObjects` on `/org/freedesktop/UDisks2` lists every block device; the ones
//! with a filesystem that udisks does not mark as system or ignored (USB sticks, SD
//! cards, loop images set up by the user) are shown. Mount, unmount and eject are the
//! udisks2 methods, so polkit decides permissions exactly as it does for Nemo.

use std::collections::HashMap;

use zbus::Connection;
use zbus::{OwnedObjectPath, OwnedVariant, Variant};

const DEST: &str = "org.freedesktop.UDisks2";
const BLOCK: &str = "org.freedesktop.UDisks2.Block";
const FILESYSTEM: &str = "org.freedesktop.UDisks2.Filesystem";
const DRIVE: &str = "org.freedesktop.UDisks2.Drive";
const LOOP: &str = "org.freedesktop.UDisks2.Loop";

type Props = HashMap<String, OwnedVariant>;
type Objects = HashMap<OwnedObjectPath, HashMap<String, Props>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// udisks object path of the block device (`/org/freedesktop/UDisks2/block_devices/sdb1`).
    pub object: String,
    pub label: String,
    /// `/dev/sdb1`
    pub device: String,
    pub size: u64,
    pub mount_point: Option<String>,
    /// udisks object path of the drive, `/` if none (loop devices).
    pub drive: String,
    pub can_eject: bool,
    pub can_power_off: bool,
}

fn get<T: TryFrom<OwnedVariant>>(p: &Props, key: &str) -> Option<T> {
    p.get(key).and_then(|v| v.try_clone().ok()).and_then(|v| T::try_from(v).ok())
}

/// A NUL-terminated byte string (`ay`) as text.
fn bytes_str(b: &[u8]) -> String {
    String::from_utf8_lossy(b.split(|&c| c == 0).next().unwrap_or_default()).into_owned()
}

/// The devices worth showing, sorted by label. Pure, so it is unit-tested.
/// `uid` is the current user: loop devices it set up (mounted ISO or disk images) are shown
/// even though udisks marks them as system devices, as gvfs does.
#[must_use]
pub fn devices_from(objects: &Objects, uid: u32) -> Vec<Device> {
    let mut out = Vec::new();
    for (path, ifaces) in objects {
        let (Some(block), Some(fs)) = (ifaces.get(BLOCK), ifaces.get(FILESYSTEM)) else { continue };
        let own_loop = ifaces.get(LOOP).and_then(|l| get::<u32>(l, "SetupByUID")) == Some(uid);
        if get::<bool>(block, "HintIgnore").unwrap_or(false)
            || (get::<bool>(block, "HintSystem").unwrap_or(true) && !own_loop)
        {
            continue; // internal disks and hidden helpers stay out of the sidebar
        }
        let size = get::<u64>(block, "Size").unwrap_or(0);
        let label = get::<String>(block, "IdLabel")
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| format!("{} Volume", core_vfs::format::size(size)));
        let mount_point = get::<Vec<Vec<u8>>>(fs, "MountPoints")
            .and_then(|m| m.first().map(|p| bytes_str(p)))
            .filter(|p| !p.is_empty());
        let drive = get::<OwnedObjectPath>(block, "Drive").map_or_else(|| "/".to_owned(), |o| o.as_str().to_owned());
        let drive_props = objects.iter().find(|(p, _)| p.as_str() == drive).and_then(|(_, i)| i.get(DRIVE));
        out.push(Device {
            object: path.as_str().to_owned(),
            label,
            device: get::<Vec<u8>>(block, "Device").map(|d| bytes_str(&d)).unwrap_or_default(),
            size,
            mount_point,
            can_eject: drive_props.and_then(|d| get::<bool>(d, "Ejectable")).unwrap_or(false),
            can_power_off: drive_props.and_then(|d| get::<bool>(d, "CanPowerOff")).unwrap_or(false),
            drive,
        });
    }
    out.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()).then(a.object.cmp(&b.object)));
    out
}

fn no_options() -> HashMap<&'static str, Variant<'static>> {
    HashMap::new()
}

/// Every removable / user filesystem udisks knows.
///
/// # Errors
/// D-Bus errors (udisks2 not running, bus unavailable).
pub async fn list(conn: &Connection) -> zbus::Result<Vec<Device>> {
    let reply = conn
        .call_method(
            Some(DEST),
            "/org/freedesktop/UDisks2",
            Some("org.freedesktop.DBus.ObjectManager"),
            "GetManagedObjects",
            &(),
        )
        .await?;
    let objects: Objects = reply.body().deserialize()?;
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    Ok(devices_from(&objects, uid))
}

/// Mount; returns the mount point udisks chose (`/run/media/$USER/<label>`).
///
/// # Errors
/// D-Bus errors, including udisks' own (not authorized, already mounted, ...).
pub async fn mount(conn: &Connection, object: &str) -> zbus::Result<String> {
    let reply = conn.call_method(Some(DEST), object, Some(FILESYSTEM), "Mount", &(no_options(),)).await?;
    reply.body().deserialize::<String>()
}

/// # Errors
/// D-Bus errors (for example "target is busy").
pub async fn unmount(conn: &Connection, object: &str) -> zbus::Result<()> {
    conn.call_method(Some(DEST), object, Some(FILESYSTEM), "Unmount", &(no_options(),)).await.map(drop)
}

/// Unmount, then eject the drive (or power it off), so it is safe to unplug.
///
/// # Errors
/// D-Bus errors from either step.
pub async fn safely_remove(conn: &Connection, dev: &Device) -> zbus::Result<()> {
    if dev.mount_point.is_some() {
        unmount(conn, &dev.object).await?;
    }
    if dev.drive != "/" {
        if dev.can_eject {
            conn.call_method(Some(DEST), dev.drive.as_str(), Some(DRIVE), "Eject", &(no_options(),)).await?;
        } else if dev.can_power_off {
            conn.call_method(Some(DEST), dev.drive.as_str(), Some(DRIVE), "PowerOff", &(no_options(),)).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ov<'a>(v: impl Into<Variant<'a>>) -> OwnedVariant {
        v.into().try_to_owned().unwrap_or_else(|e| panic!("{e}"))
    }

    fn path(p: &str) -> OwnedObjectPath {
        OwnedObjectPath::try_from(p).unwrap_or_else(|e| panic!("{e}"))
    }

    fn cstr(s: &str) -> Vec<u8> {
        let mut v = s.as_bytes().to_vec();
        v.push(0);
        v
    }

    #[test]
    fn picks_user_filesystems_and_reads_their_state() {
        let mut objects: Objects = HashMap::new();
        let block = |label: &str, system: bool, drive: &str| -> Props {
            [
                ("IdLabel".to_owned(), ov(label)),
                ("HintSystem".to_owned(), ov(system)),
                ("HintIgnore".to_owned(), ov(false)),
                ("Size".to_owned(), ov(16_000_000_000u64)),
                ("Device".to_owned(), ov(cstr("/dev/sdx1"))),
                ("Drive".to_owned(), ov(path(drive))),
            ]
            .into()
        };
        let fs = |mounts: Vec<Vec<u8>>| -> Props { [("MountPoints".to_owned(), ov(mounts))].into() };
        objects.insert(
            path("/org/freedesktop/UDisks2/block_devices/sdx1"),
            [
                (BLOCK.to_owned(), block("", false, "/org/freedesktop/UDisks2/drives/demo_stick")),
                (FILESYSTEM.to_owned(), fs(vec![cstr("/run/media/user/STICK")])),
            ]
            .into(),
        );
        objects.insert(
            path("/org/freedesktop/UDisks2/block_devices/nvme0n1p2"),
            [(BLOCK.to_owned(), block("system_root", true, "/")), (FILESYSTEM.to_owned(), fs(vec![cstr("/")]))].into(),
        );
        objects.insert(
            path("/org/freedesktop/UDisks2/block_devices/sdy1"),
            [(BLOCK.to_owned(), block("BACKUP_01", false, "/")), (FILESYSTEM.to_owned(), fs(Vec::new()))].into(),
        );
        objects.insert(
            path("/org/freedesktop/UDisks2/drives/demo_stick"),
            [(DRIVE.to_owned(), [("Ejectable".to_owned(), ov(true)), ("CanPowerOff".to_owned(), ov(true))].into())]
                .into(),
        );
        objects.insert(
            path("/org/freedesktop/UDisks2/block_devices/loop7"),
            [
                (BLOCK.to_owned(), block("IMAGE_ISO", true, "/")),
                (FILESYSTEM.to_owned(), fs(Vec::new())),
                (LOOP.to_owned(), [("SetupByUID".to_owned(), ov(1000u32))].into()),
            ]
            .into(),
        );
        let d = devices_from(&objects, 1000);
        assert_eq!(d.len(), 3, "the system disk is left out, the user's own loop image is shown");
        assert_eq!(devices_from(&objects, 1001).len(), 2, "another user's loop device is not");
        assert!(d.iter().any(|x| x.label == "IMAGE_ISO"));
        let d: Vec<Device> = d.into_iter().filter(|x| x.label != "IMAGE_ISO").collect();
        assert_eq!(d[0].label, "16.0 GB Volume", "unlabelled volumes are named by size");
        assert_eq!(d[0].mount_point.as_deref(), Some("/run/media/user/STICK"));
        assert_eq!(d[0].device, "/dev/sdx1");
        assert!(d[0].can_eject && d[0].can_power_off);
        assert_eq!((d[1].label.as_str(), d[1].mount_point.as_deref(), d[1].can_eject), ("BACKUP_01", None, false));
    }
}
