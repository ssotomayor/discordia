use std::path::{Path, PathBuf};

pub const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "avif", "bmp"];
/// For pictures stored as picked (stickers, event covers): what the server keeps and this
/// client can read. No BMP, which the store refuses, nor AVIF, which `image` here cannot open.
pub const STORED_IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];

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
            dialog = dialog.add_filter(*name, &either_case(extensions));
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

/// GTK matches a filter's patterns case-sensitively, so `PHOTO.JPG` would be greyed
/// out on Linux; macOS and Windows ignore case already.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn either_case(extensions: &[&str]) -> Vec<String> {
    let mut all: Vec<String> = extensions.iter().map(|e| e.to_string()).collect();
    if cfg!(target_os = "linux") {
        all.extend(extensions.iter().map(|e| e.to_ascii_uppercase()));
    }
    all
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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    #[test]
    fn linux_filters_offer_both_cases() {
        assert_eq!(
            super::either_case(&["jpg", "png"]),
            ["jpg", "png", "JPG", "PNG"]
        );
    }
}
