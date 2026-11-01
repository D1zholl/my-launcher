use std::{fs, io::Write, path::Path};

/// Writes `contents` through a temporary file and keeps a backup until the
/// replacement succeeded.
///
/// `fs::rename` does not overwrite an existing destination on Windows, so a
/// stale backup left behind by a previous crash must be removed first.
pub(crate) fn write_file_with_backup(path: &Path, contents: &[u8]) -> Result<(), String> {
    let file_name = path
        .file_name()
        .ok_or_else(|| "Некорректный путь файла.".to_string())?
        .to_string_lossy()
        .into_owned();
    let parent = path
        .parent()
        .ok_or_else(|| "Некорректная папка файла.".to_string())?;
    let temporary = parent.join(format!("{file_name}.tmp"));
    let backup = parent.join(format!("{file_name}.backup"));

    {
        let mut file = fs::File::create(&temporary).map_err(|error| error.to_string())?;
        file.write_all(contents).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
    }

    if path.exists() {
        if backup.exists() {
            fs::remove_file(&backup).map_err(|error| error.to_string())?;
        }
        fs::rename(path, &backup).map_err(|error| error.to_string())?;
    }

    if let Err(error) = fs::rename(&temporary, path) {
        if backup.exists() {
            let _ = fs::rename(&backup, path);
        }
        let _ = fs::remove_file(&temporary);
        return Err(error.to_string());
    }

    if backup.exists() {
        fs::remove_file(&backup).map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::write_file_with_backup;
    use std::fs;

    #[test]
    fn replaces_stale_backup_left_by_a_crash() {
        let root = std::env::current_dir()
            .unwrap()
            .join(format!(".mylauncher-atomic-write-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join(".launcher_meta.json");
        fs::write(&path, b"old").unwrap();
        fs::write(root.join(".launcher_meta.json.backup"), b"stale").unwrap();

        write_file_with_backup(&path, b"new").unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert!(!root.join(".launcher_meta.json.backup").exists());
        assert!(!root.join(".launcher_meta.json.tmp").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
