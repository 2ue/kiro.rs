use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Context;

fn temp_path_for(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("atomic-write");
    parent.join(format!(
        ".{}.tmp-{}-{}",
        file_name,
        std::process::id(),
        uuid::Uuid::new_v4()
    ))
}

/// Atomically replace a file by writing to a same-directory temp file first.
pub fn write_file_atomic(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> anyhow::Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("创建目录失败: {}", parent.display()))?;
        }
    }

    let temp_path = temp_path_for(path);
    let write_result = (|| -> anyhow::Result<()> {
        let mut file = File::create(&temp_path)
            .with_context(|| format!("创建临时文件失败: {}", temp_path.display()))?;
        file.write_all(contents.as_ref())
            .with_context(|| format!("写入临时文件失败: {}", temp_path.display()))?;
        file.sync_all()
            .with_context(|| format!("同步临时文件失败: {}", temp_path.display()))?;
        fs::rename(&temp_path, path).with_context(|| {
            format!(
                "原子替换文件失败: {} -> {}",
                temp_path.display(),
                path.display()
            )
        })?;
        Ok(())
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }

    write_result
}
