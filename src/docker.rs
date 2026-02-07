use std::process::Command;

use crate::error::{KleviathanError, KleviathanResult};

fn check_config_exists() -> KleviathanResult<String> {
    let home = std::env::var("HOME")
        .map_err(|_| KleviathanError::Docker("HOME environment variable not set".into()))?;
    let config_path = format!("{home}/.kleviathan/kleviathan.jsonc");
    if !std::path::Path::new(&config_path).exists() {
        return Err(KleviathanError::Docker(format!(
            "Configuration file not found at {config_path}\nRun 'kleviathan make-config' first to create it."
        )));
    }
    Ok(format!("{home}/.kleviathan"))
}

fn build_image() -> KleviathanResult<()> {
    eprintln!("Building Docker image...");
    let build = Command::new("docker")
        .args(["build", "-t", "kleviathan", "."])
        .status()
        .map_err(|e| KleviathanError::Docker(format!("Failed to build image: {e}")))?;
    if !build.success() {
        return Err(KleviathanError::Docker("Docker build failed".into()));
    }

    Ok(())
}

fn stop_existing_container() -> KleviathanResult<()> {
    let output = Command::new("docker")
        .args(["ps", "-q", "--filter", "name=kleviathan"])
        .output()
        .map_err(|e| KleviathanError::Docker(format!("Failed to check running containers: {e}")))?;

    let id = String::from_utf8_lossy(&output.stdout);
    if !id.trim().is_empty() {
        eprintln!("Stopping existing container...");
        let _ = Command::new("docker")
            .args(["stop", "kleviathan"])
            .status();
        let _ = Command::new("docker")
            .args(["rm", "kleviathan"])
            .status();
    }

    Ok(())
}

fn run_container(config_dir: &str) -> KleviathanResult<()> {
    let volume = format!("{config_dir}:/home/kleviathan/.kleviathan");
    let status = Command::new("docker")
        .args([
            "run",
            "--name", "kleviathan",
            "--rm",
            "-v", &volume,
            "kleviathan",
            "run-inner",
        ])
        .status()
        .map_err(|e| KleviathanError::Docker(format!("Failed to run container: {e}")))?;

    if !status.success() {
        return Err(KleviathanError::Docker(format!(
            "Container exited with status: {status}"
        )));
    }

    Ok(())
}

pub fn run() -> KleviathanResult<()> {
    let config_dir = check_config_exists()?;
    build_image()?;
    stop_existing_container()?;
    run_container(&config_dir)?;
    Ok(())
}
