use std::path::{Path, PathBuf};

pub(super) fn try_save_clipboard_image(cwd: &Path) -> Option<String> {
    let mut clipboard = arboard::Clipboard::new().ok()?;
    let img = clipboard.get_image().ok()?;
    let width = u32::try_from(img.width).ok()?;
    let height = u32::try_from(img.height).ok()?;
    let buf = image::RgbaImage::from_raw(width, height, img.bytes.into_owned())?;

    let dir = cwd.join(".nav").join("clipboard");
    std::fs::create_dir_all(&dir).ok()?;
    let filename = format!("{}.png", uuid::Uuid::new_v4().simple());
    let abs = dir.join(&filename);
    image::DynamicImage::ImageRgba8(buf)
        .save_with_format(&abs, image::ImageFormat::Png)
        .ok()?;

    let rel = PathBuf::from(".nav").join("clipboard").join(filename);
    Some(rel.to_string_lossy().into_owned())
}

pub(super) fn workspace_relative_image(cwd: &Path, cleaned: &str) -> Option<String> {
    let path = Path::new(cleaned);
    let abs_for_check = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let canonical = abs_for_check
        .canonicalize()
        .unwrap_or_else(|_| abs_for_check.clone());
    let cwd_canonical = cwd.canonicalize().ok().unwrap_or_else(|| cwd.to_path_buf());

    if let Ok(rel) = canonical.strip_prefix(&cwd_canonical) {
        return Some(rel.to_string_lossy().into_owned());
    }

    let dir = cwd.join(".nav").join("clipboard");
    std::fs::create_dir_all(&dir).ok()?;
    let ext = canonical
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("png");
    let filename = format!("{}.{ext}", uuid::Uuid::new_v4().simple());
    let dest = dir.join(&filename);
    std::fs::copy(&canonical, &dest).ok()?;
    let rel = PathBuf::from(".nav").join("clipboard").join(filename);
    Some(rel.to_string_lossy().into_owned())
}

pub(super) fn recognized_image_path(s: &str) -> Option<String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    let path: PathBuf = if let Ok(url) = url::Url::parse(trimmed)
        && url.scheme() == "file"
    {
        url.to_file_path().ok()?
    } else {
        PathBuf::from(trimmed)
    };
    let ext = path.extension().and_then(|e| e.to_str())?;
    image::ImageFormat::from_extension(ext)?;
    image::image_dimensions(&path).ok()?;
    Some(path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognized_image_path_rejects_non_image_text() {
        assert_eq!(recognized_image_path("just some text"), None);
        assert_eq!(recognized_image_path(""), None);
        assert_eq!(recognized_image_path("/etc/passwd"), None);
    }

    #[test]
    fn recognized_image_path_rejects_nonexistent_image_extension() {
        assert_eq!(recognized_image_path("/tmp/does-not-exist.png"), None);
    }

    #[test]
    fn recognized_image_path_accepts_real_png_and_strips_file_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.png");
        write_png(&path);

        let path_str = path.to_string_lossy().into_owned();
        assert_eq!(recognized_image_path(&path_str), Some(path_str.clone()));
        let file_url = format!("file://{}", path_str);
        assert_eq!(recognized_image_path(&file_url), Some(path_str));
    }

    #[test]
    fn recognized_image_path_decodes_percent_encoded_file_url() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("My Image.png");
        write_png(&path);

        let url = url::Url::from_file_path(&path).expect("valid file path");
        let encoded = url.as_str();
        assert!(
            encoded.contains("%20"),
            "expected encoded space in test fixture: {encoded}"
        );

        let decoded = recognized_image_path(encoded).expect("encoded file URL must resolve");
        assert_eq!(decoded, path.to_string_lossy());
    }

    #[test]
    fn workspace_relative_passes_relative_through() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("screenshots").join("foo.png");
        std::fs::create_dir_all(png.parent().unwrap()).unwrap();
        write_png(&png);
        let out = workspace_relative_image(dir.path(), "screenshots/foo.png").unwrap();
        assert_eq!(out, "screenshots/foo.png");
    }

    #[test]
    fn workspace_relative_strips_cwd_prefix_for_in_workspace_paths() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("a").join("b.png");
        std::fs::create_dir_all(png.parent().unwrap()).unwrap();
        write_png(&png);
        let out = workspace_relative_image(dir.path(), &png.to_string_lossy()).unwrap();
        assert_eq!(out, "a/b.png");
    }

    #[test]
    fn workspace_relative_copies_external_path_into_clipboard_dir() {
        let src_dir = tempfile::tempdir().unwrap();
        let src = src_dir.path().join("outside.png");
        write_png(&src);

        let cwd = tempfile::tempdir().unwrap();
        let out = workspace_relative_image(cwd.path(), &src.to_string_lossy()).unwrap();
        assert!(
            out.starts_with(".nav/clipboard/") && out.ends_with(".png"),
            "expected workspace-relative copy, got {out:?}"
        );
        assert!(cwd.path().join(&out).exists());
    }

    #[test]
    fn workspace_relative_copies_relative_path_that_escapes_cwd() {
        let outer = tempfile::tempdir().unwrap();
        let outside = outer.path().join("escapes.png");
        write_png(&outside);
        let cwd = outer.path().join("workspace");
        std::fs::create_dir_all(&cwd).unwrap();

        let out = workspace_relative_image(&cwd, "../escapes.png").unwrap();
        assert!(
            out.starts_with(".nav/clipboard/") && out.ends_with(".png"),
            "relative escape must be copied in, got {out:?}"
        );
        assert!(cwd.join(&out).exists());
    }

    fn write_png(path: &Path) {
        let img = image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 0]));
        image::DynamicImage::ImageRgba8(img)
            .save_with_format(path, image::ImageFormat::Png)
            .unwrap();
    }
}
