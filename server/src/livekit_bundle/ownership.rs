use std::fs::{File, OpenOptions};
use std::path::Path;

fn claim(path: &Path) -> Result<File, String> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|e| format!("cannot open host ownership lock: {e}"))?;
    file.try_lock().map_err(|_| "Another host is using this data directory or media port. Use a separate configuration directory and a different DISCORDIA_LIVEKIT_PORT block.".to_string())?;
    Ok(file)
}

pub struct HostLease {
    _directory: File,
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
    _directory: File,
    _ports: Vec<File>,
}

impl MediaOwnership {
    pub(super) fn claim(data_dir: &Path, ports: super::LivekitPorts) -> Result<Self, String> {
        Self::claim_in(
            data_dir,
            &std::env::temp_dir().join("discordia-media-ownership"),
            ports,
        )
    }

    fn claim_in(
        data_dir: &Path,
        lock_dir: &Path,
        ports: super::LivekitPorts,
    ) -> Result<Self, String> {
        super::private_dir(data_dir).map_err(|e| format!("media directory: {e}"))?;
        super::private_dir(lock_dir).map_err(|e| format!("media ownership directory: {e}"))?;
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
