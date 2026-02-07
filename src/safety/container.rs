use std::path::Path;

use crate::error::{KleviathanError, KleviathanResult};

pub fn enforce_container() -> KleviathanResult<()> {
    let has_dockerenv = Path::new("/.dockerenv").exists();
    let has_cgroup = std::fs::read_to_string("/proc/1/cgroup")
        .map(|content| content.contains("docker") || content.contains("containerd"))
        .unwrap_or(false);
    if !has_dockerenv && !has_cgroup {
        return Err(KleviathanError::NotInContainer(
            "Kleviathan must run inside a Docker container".to_string(),
        ));
    }
    Ok(())
}
