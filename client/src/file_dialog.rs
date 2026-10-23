use std::path::{Path, PathBuf};

pub struct PickedFile {
    path: PathBuf,
}

impl std::ops::Deref for PickedFile {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

#[cfg(target_os = "android")]
impl Drop for PickedFile {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.path) {
            tracing::warn!(%error, "Couldn't remove temporary document");
            return;
        }
        if let Some(parent) = self.path.parent()
            && let Err(error) = std::fs::remove_dir(parent)
        {
            tracing::warn!(%error, "Couldn't remove temporary document folder");
        }
    }
}

pub async fn pick_file(filters: &[(&str, &[&str])]) -> Result<Option<PickedFile>, String> {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        let mut dialog = rfd::AsyncFileDialog::new();
        for (name, extensions) in filters {
            dialog = dialog.add_filter(*name, extensions);
        }
        Ok(dialog.pick_file().await.map(|file| PickedFile {
            path: file.path().to_owned(),
        }))
    }
    #[cfg(target_os = "android")]
    {
        let extensions = filters
            .iter()
            .flat_map(|(_, extensions)| extensions.iter().copied())
            .collect::<Vec<_>>()
            .join(",");
        let result = crate::android::document(String::new(), extensions, None).await?;
        Ok(result["path"].as_str().map(|path| PickedFile {
            path: PathBuf::from(path),
        }))
    }
    #[cfg(target_os = "ios")]
    {
        let _ = filters;
        Err("File selection is not available on mobile yet.".into())
    }
}

#[cfg(not(target_os = "android"))]
pub async fn save_file(name: &str) -> Result<Option<PathBuf>, String> {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        Ok(rfd::AsyncFileDialog::new()
            .set_file_name(name)
            .save_file()
            .await
            .map(|file| file.path().to_owned()))
    }
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        let _ = name;
        Err("Saving attachments is not available on mobile yet.".into())
    }
}

pub async fn pick_folder(start: &Path) -> Result<Option<PathBuf>, String> {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        let mut dialog = rfd::AsyncFileDialog::new();
        if start.is_dir() {
            dialog = dialog.set_directory(start);
        }
        Ok(dialog
            .pick_folder()
            .await
            .map(|folder| folder.path().to_owned()))
    }
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        let _ = start;
        Err("Custom storage folders are not available on mobile.".into())
    }
}
