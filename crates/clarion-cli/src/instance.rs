use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use uuid::Uuid;

const INSTANCE_ID_FILE: &str = "instance_id";

pub fn load_or_create(project_root: &Path) -> Result<String> {
    let path = project_root.join(".clarion").join(INSTANCE_ID_FILE);
    match fs::read_to_string(&path) {
        Ok(raw) => read_existing_instance_id(&path, &raw),
        Err(err) if err.kind() == io::ErrorKind::NotFound => create_instance_id(&path),
        Err(err) => Err(err).with_context(|| format!("read {}", path.display())),
    }
}

fn create_instance_id(path: &Path) -> Result<String> {
    let instance_id = Uuid::new_v4().to_string();
    let temp_path = path.with_file_name(format!(".{INSTANCE_ID_FILE}.{instance_id}.tmp"));
    let mut file = create_new_private_file(&temp_path)
        .with_context(|| format!("create temporary {}", temp_path.display()))?;
    writeln!(file, "{instance_id}").with_context(|| format!("write {}", temp_path.display()))?;
    file.sync_all()
        .with_context(|| format!("sync {}", temp_path.display()))?;
    drop(file);

    match fs::hard_link(&temp_path, path) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            let _ = fs::remove_file(&temp_path);
            let raw = fs::read_to_string(path)
                .with_context(|| format!("read concurrently-created {}", path.display()))?;
            return read_existing_instance_id(path, &raw);
        }
        Err(err) => {
            let _ = fs::remove_file(&temp_path);
            return Err(err).with_context(|| {
                format!(
                    "publish temporary {} to {}",
                    temp_path.display(),
                    path.display()
                )
            });
        }
    }
    fs::remove_file(&temp_path).with_context(|| format!("remove {}", temp_path.display()))?;
    #[cfg(unix)]
    set_private_mode(path)?;
    Ok(instance_id)
}

#[cfg(unix)]
fn create_new_private_file(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn create_new_private_file(path: &Path) -> io::Result<fs::File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

#[cfg(unix)]
fn set_private_mode(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)
        .with_context(|| format!("stat {}", path.display()))?
        .permissions();
    permissions.set_mode(0o600);
    fs::set_permissions(path, permissions)
        .with_context(|| format!("set private mode on {}", path.display()))
}

#[cfg(not(unix))]
fn set_private_mode(_path: &Path) -> Result<()> {
    Ok(())
}

fn read_existing_instance_id(path: &Path, raw: &str) -> Result<String> {
    let instance_id = parse_instance_id(path, raw)?;
    set_private_mode(path)?;
    Ok(instance_id)
}

fn parse_instance_id(path: &Path, raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    let id = Uuid::parse_str(trimmed).map_err(|err| invalid_instance_id(path, &err))?;
    Ok(id.to_string())
}

fn invalid_instance_id(path: &Path, source: &uuid::Error) -> anyhow::Error {
    anyhow!(
        "invalid Clarion instance ID in {}: {source}; expected a UUID",
        path.display()
    )
}
