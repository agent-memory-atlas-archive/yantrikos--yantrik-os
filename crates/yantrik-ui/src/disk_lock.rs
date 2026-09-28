//! Whether this boot already asked for a secret: the root filesystem is on LUKS, and its
//! crypttab entry has nothing that could answer the passphrase prompt by itself.
//!
//! Then the desktop does not lock at start (#400 step b). Whoever got past the disk's prompt knew
//! the passphrase, and asking again straight after is the second password Omarchy also skips. A
//! reboot, which is what the lock at start is there to stop (#415), cannot get past the disk.
//!
//! Anything less certain keeps the lock: a key file, a keyscript, a TPM, FIDO or PKCS#11 token,
//! Clevis, a root on LVM or on anything that is not a LUKS mapping directly, or files that cannot
//! be read. Automatic unlock would make a reboot free again, so it must never read as "asked".

use std::path::Path;

/// True when the root was opened at this boot by a passphrase someone typed.
pub fn root_unlocked_by_passphrase() -> bool {
    let Ok(mountinfo) = std::fs::read_to_string("/proc/self/mountinfo") else { return false };
    let Some(source) = root_source(&mountinfo) else { return false };
    // /dev/mapper/yantrik-root is a link to /dev/dm-N, whose sysfs says what it is.
    let Ok(device) = std::fs::canonicalize(&source) else { return false };
    let Some(dm) = device.file_name().and_then(|n| n.to_str()).filter(|n| n.starts_with("dm-")) else {
        return false;
    };
    let sys = Path::new("/sys/block").join(dm).join("dm");
    let read = |f: &str| std::fs::read_to_string(sys.join(f)).map(|s| s.trim().to_string());
    let (Ok(name), Ok(uuid)) = (read("name"), read("uuid")) else { return false };
    if !uuid.starts_with("CRYPT-LUKS") {
        return false;
    }
    // Clevis unlocks through its own initramfs hook, with `none` left in crypttab.
    if ["/usr/bin/clevis", "/usr/bin/clevis-luks-unlock"].iter().any(|p| Path::new(p).exists()) {
        return false;
    }
    let crypttab = std::fs::read_to_string("/etc/crypttab").unwrap_or_default();
    asks_for_passphrase(&crypttab, &name)
}

/// The device mounted at `/`, from /proc/self/mountinfo: the last entry for `/` wins, as the
/// last mount over a point is the one in use.
pub fn root_source(mountinfo: &str) -> Option<String> {
    mountinfo
        .lines()
        .filter_map(|line| {
            let (before, after) = line.split_once(" - ")?;
            let mount_point = before.split_whitespace().nth(4)?;
            let source = after.split_whitespace().nth(1)?;
            (mount_point == "/").then(|| source.to_string())
        })
        .last()
}

/// Whether `/etc/crypttab`'s entry for `name` is opened by a passphrase typed at boot: no key
/// file (`none`, `-` or absent) and no option that supplies one another way.
pub fn asks_for_passphrase(crypttab: &str, name: &str) -> bool {
    const ANSWERS_ITSELF: &[&str] =
        &["keyscript", "tpm2-device", "fido2-device", "pkcs11-uri", "tpm2-", "fido2-", "token-"];
    let Some(fields) = crypttab
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.split_whitespace().collect::<Vec<_>>())
        .find(|f| f.first() == Some(&name))
    else {
        return false;
    };
    let key_file = fields.get(2).copied().unwrap_or("none");
    if key_file != "none" && key_file != "-" {
        return false;
    }
    let options = fields.get(3).copied().unwrap_or("");
    !options
        .split(',')
        .any(|o| ANSWERS_ITSELF.iter().any(|a| o.trim().starts_with(a)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENCRYPTED: &str = "\
22 1 0:21 / /sys rw,nosuid,nodev,noexec,relatime shared:7 - sysfs sysfs rw
26 1 254:0 / / rw,noatime shared:1 - ext4 /dev/mapper/yantrik-root rw
27 26 8:2 / /boot rw,noatime shared:2 - ext4 /dev/sda2 rw
";

    #[test]
    fn the_root_is_found_among_the_mounts() {
        assert_eq!(root_source(ENCRYPTED).as_deref(), Some("/dev/mapper/yantrik-root"));
        let plain = "25 1 8:2 / / rw,noatime shared:1 - ext4 /dev/sda2 rw,errors=remount-ro\n";
        assert_eq!(root_source(plain).as_deref(), Some("/dev/sda2"));
        // A live session: the overlay mounted over / last is the root.
        let live = "20 1 0:30 / / rw - squashfs /dev/loop0 ro\n30 1 0:40 / / rw - overlay overlay rw\n";
        assert_eq!(root_source(live).as_deref(), Some("overlay"));
        assert_eq!(root_source(""), None);
    }

    #[test]
    fn the_installers_entry_asks_for_a_passphrase() {
        let tab = "# <target> <source> <key> <options>\nyantrik-root UUID=0f0e none luks,initramfs\n";
        assert!(asks_for_passphrase(tab, "yantrik-root"));
        assert!(asks_for_passphrase("yantrik-root UUID=0f0e\n", "yantrik-root"), "no key field");
        assert!(asks_for_passphrase("yantrik-root UUID=0f0e - luks\n", "yantrik-root"));
    }

    #[test]
    fn anything_that_can_answer_by_itself_keeps_the_lock() {
        for entry in [
            "yantrik-root UUID=0f0e /etc/keys/root.key luks",
            "yantrik-root UUID=0f0e none luks,keyscript=/lib/cryptsetup/scripts/passdev",
            "yantrik-root UUID=0f0e none luks,tpm2-device=auto",
            "yantrik-root UUID=0f0e none luks,fido2-device=auto",
            "yantrik-root UUID=0f0e none luks,pkcs11-uri=auto",
            "yantrik-root UUID=0f0e none luks,token-timeout=0",
        ] {
            assert!(!asks_for_passphrase(entry, "yantrik-root"), "{entry}");
        }
    }

    #[test]
    fn a_mapping_crypttab_does_not_name_keeps_the_lock() {
        let tab = "other UUID=1 none luks\n#yantrik-root UUID=0f0e none luks\n";
        assert!(!asks_for_passphrase(tab, "yantrik-root"));
        assert!(!asks_for_passphrase("", "yantrik-root"));
    }
}
