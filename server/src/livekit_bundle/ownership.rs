use std::fs::{File, OpenOptions};
use std::path::Path;

struct OwnershipLock {
    file: File,
}

impl Drop for OwnershipLock {
    fn drop(&mut self) {
        // A forked child can retain the lock through a duplicate until exec closes it.
        if let Err(error) = self.file.unlock() {
            tracing::warn!(%error, "cannot release host ownership lock");
        }
    }
}

fn claim(path: &Path) -> Result<OwnershipLock, String> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|e| format!("cannot open host ownership lock: {e}"))?;
    file.try_lock().map_err(|_| "Another host is using this data directory or media port. Use a separate configuration directory and a different DISCORDIA_LIVEKIT_PORT block.".to_string())?;
    Ok(OwnershipLock { file })
}

/// Shared by this user's hosts and nobody else's: another user's SFU on the
/// same port already fails to bind, and a directory others can write to was
/// theirs to fill first.
fn lock_root() -> std::path::PathBuf {
    #[cfg(unix)]
    {
        if let Some(run) = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
            return std::path::PathBuf::from(run).join("discordia-media-ownership");
        }
        // SAFETY: geteuid has no preconditions and cannot fail.
        let uid = unsafe { libc::geteuid() };
        std::env::temp_dir().join(format!("discordia-media-ownership-{uid}"))
    }
    #[cfg(not(unix))]
    std::env::temp_dir().join("discordia-media-ownership")
}

/// A private directory that is a real one and this user's; a name someone
/// else created first, or a symlink, is refused rather than used.
fn owned_private_dir(dir: &Path) -> std::io::Result<()> {
    super::private_dir(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::symlink_metadata(dir)?;
        // SAFETY: geteuid has no preconditions and cannot fail.
        let me = unsafe { libc::geteuid() };
        if !meta.file_type().is_dir() || meta.uid() != me {
            return Err(std::io::Error::other(format!(
                "{} is not a directory this user owns",
                dir.display()
            )));
        }
    }
    Ok(())
}

pub struct HostLease {
    _directory: OwnershipLock,
}

impl HostLease {
    pub fn claim(data_dir: &Path) -> Result<Self, String> {
        super::private_dir(data_dir).map_err(|e| format!("host directory: {e}"))?;
        Ok(Self {
            _directory: claim(&data_dir.join("host.lock"))?,
        })
    }
}

pub(super) struct MediaOwnership {
    _directory: OwnershipLock,
    _ports: Vec<OwnershipLock>,
}

impl MediaOwnership {
    pub(super) fn claim(data_dir: &Path, ports: super::LivekitPorts) -> Result<Self, String> {
        Self::claim_in(data_dir, &lock_root(), ports)
    }

    fn claim_in(
        data_dir: &Path,
        lock_dir: &Path,
        ports: super::LivekitPorts,
    ) -> Result<Self, String> {
        super::private_dir(data_dir).map_err(|e| format!("media directory: {e}"))?;
        owned_private_dir(lock_dir).map_err(|e| format!("media ownership directory: {e}"))?;
        let directory = claim(&data_dir.join("owner.lock"))?;
        let mut locks = Vec::new();
        for (protocol, port) in [("tcp", ports.ws), ("tcp", ports.tcp), ("udp", ports.udp)] {
            locks.push(claim(&lock_dir.join(format!("{protocol}-{port}.lock")))?);
        }
        Ok(Self {
            _directory: directory,
            _ports: locks,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("discordia-media-owners-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn dropping_ownership_unlocks_while_an_inherited_descriptor_survives() {
        let root = scratch();
        super::super::private_dir(&root).unwrap();
        let path = root.join("owner.lock");
        let owner = claim(&path).unwrap();
        // A duplicate models the descriptor a concurrent fork inherits before exec.
        let inherited = owner.file.try_clone().unwrap();
        assert!(claim(&path).is_err());
        drop(owner);
        let restarted = claim(&path).unwrap();
        assert!(claim(&path).is_err());
        drop(inherited);
        assert!(claim(&path).is_err());
        drop(restarted);
        drop(claim(&path).unwrap());
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn concurrent_hosts_require_separate_directories_and_keep_separate_credentials() {
        let root = scratch();
        let first = root.join("first");
        let second = root.join("second");
        let one = HostLease::claim(&first).unwrap();
        assert!(HostLease::claim(&first).is_err());
        let two = HostLease::claim(&second).unwrap();
        let first_creds = super::super::credentials(&first).unwrap();
        let second_creds = super::super::credentials(&second).unwrap();
        assert_ne!(first_creds, second_creds);
        drop(one);
        let restarted = HostLease::claim(&first).unwrap();
        assert_eq!(super::super::credentials(&first).unwrap(), first_creds);
        assert!(HostLease::claim(&second).is_err());
        drop((two, restarted));
    }

    #[cfg(unix)]
    #[test]
    fn a_lock_directory_that_is_a_symlink_is_refused() {
        let root = scratch();
        super::super::private_dir(&root).unwrap();
        let elsewhere = root.join("elsewhere");
        super::super::private_dir(&elsewhere).unwrap();
        let planted = root.join("locks");
        std::os::unix::fs::symlink(&elsewhere, &planted).unwrap();
        assert!(
            MediaOwnership::claim_in(
                &root.join("host"),
                &planted,
                super::super::ports_from(Some("17950"))
            )
            .is_err(),
            "a planted symlink was taken for the lock directory"
        );
        assert!(
            std::fs::read_dir(&elsewhere).unwrap().next().is_none(),
            "nothing was written through it"
        );
        assert!(
            lock_root()
                .to_string_lossy()
                .contains("discordia-media-ownership")
        );
        assert_ne!(
            lock_root(),
            std::env::temp_dir().join("discordia-media-ownership"),
            "per user"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn media_ownership_rejects_overlaps_and_stopping_one_does_not_release_another() {
        let root = scratch();
        let locks = root.join("ports");
        let first = root.join("first");
        let second = root.join("second");
        let one = MediaOwnership::claim_in(&first, &locks, super::super::ports_from(Some("17880")))
            .unwrap();
        assert!(
            MediaOwnership::claim_in(&first, &locks, super::super::ports_from(Some("17890")))
                .is_err()
        );
        assert!(
            MediaOwnership::claim_in(&second, &locks, super::super::ports_from(Some("17881")))
                .is_err()
        );
        let two =
            MediaOwnership::claim_in(&second, &locks, super::super::ports_from(Some("17890")))
                .unwrap();
        drop(one);
        let restarted =
            MediaOwnership::claim_in(&first, &locks, super::super::ports_from(Some("17880")))
                .unwrap();
        assert!(
            MediaOwnership::claim_in(
                &root.join("third"),
                &locks,
                super::super::ports_from(Some("17890"))
            )
            .is_err()
        );
        drop(two);
        let third = MediaOwnership::claim_in(
            &root.join("third"),
            &locks,
            super::super::ports_from(Some("17890")),
        )
        .unwrap();
        drop((restarted, third));
    }
}
