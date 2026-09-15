use image::DynamicImage;
use md5::{Digest, Md5};
use std::error::Error;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::UNIX_EPOCH;
use tempfile::NamedTempFile;
use url::Url;
use walkdir::WalkDir;

/// Implements thumbnail caching based on the freedesktop.org Thumbnail Managing Standard.
/// <https://specifications.freedesktop.org/thumbnail-spec/latest>/
pub struct ThumbnailCacher {
    file_path: PathBuf,
    file_uri: String,
    thumbnail_dir: PathBuf,
    thumbnail_path: PathBuf,
    thumbnail_size: ThumbnailSize,
    thumbnail_fail_marker_path: PathBuf,
}

impl ThumbnailCacher {
    pub fn new(file_path: &Path, thumbnail_size: ThumbnailSize) -> Result<Self, String> {
        let file_uri = thumbnail_uri(file_path)
            .map_err(|err| format!("failed to create URI for {}: {}", file_path.display(), err))?;
        let cache_base_dir = THUMBNAIL_CACHE_BASE_DIR
            .as_ref()
            .ok_or("failed to get thumbnail cache directory".to_string())?;
        let thumbnail_filename = thumbnail_cache_filename(&file_uri);
        let thumbnail_dir = cache_base_dir.join(thumbnail_size.subdirectory_name());
        Self::ensure_private_directory(&thumbnail_dir).map_err(|err| {
            format!(
                "failed to prepare thumbnail cache directory {}: {err}",
                thumbnail_dir.display()
            )
        })?;
        let thumbnail_path = thumbnail_dir.join(&thumbnail_filename);
        let thumbnail_fail_marker_path = fail_marker_path(cache_base_dir, &thumbnail_filename);

        Ok(Self {
            file_path: file_path.to_path_buf(),
            file_uri,
            thumbnail_dir,
            thumbnail_path,
            thumbnail_size,
            thumbnail_fail_marker_path,
        })
    }

    pub fn get_cached_thumbnail(&self) -> CachedThumbnail {
        // If the file is already a thumbnail, just use it so we don't generate
        // cached thumbnails of thumbnails.
        if let (Some(cache_base_dir), Ok(metadata)) = (
            THUMBNAIL_CACHE_BASE_DIR.as_ref(),
            std::fs::metadata(&self.file_path),
        ) && metadata.is_file()
            && self.file_path.starts_with(cache_base_dir)
        {
            return CachedThumbnail::Valid((self.file_path.clone(), None));
        }

        // Use cached thumbnail if it is valid.
        if self.is_thumbnail_valid(&self.thumbnail_path) {
            return CachedThumbnail::Valid((
                self.thumbnail_path.clone(),
                Some(self.thumbnail_size),
            ));
        }

        // Check if there is a fail marker from an earlier failure.
        if self.is_thumbnail_valid(&self.thumbnail_fail_marker_path) {
            return CachedThumbnail::Failed;
        }

        CachedThumbnail::RequiresUpdate(self.thumbnail_size)
    }

    pub fn thumbnail_dir(&self) -> &Path {
        &self.thumbnail_dir
    }

    pub fn update_with_temp_file(&self, temp_file: NamedTempFile) -> Result<&Path, Box<dyn Error>> {
        self.update_from_file(temp_file.path())?;
        Ok(&self.thumbnail_path)
    }

    pub fn update_from_file(&self, thumbnail_file: &Path) -> Result<(), Box<dyn Error>> {
        let file = File::open(thumbnail_file)?;
        let mut decoder = png::Decoder::new_with_limits(
            BufReader::new(file),
            png::Limits {
                bytes: self.maximum_decoded_bytes(),
            },
        );
        // Replace untrusted text with source-derived cache metadata.
        decoder.set_ignore_text_chunk(true);
        let mut reader = decoder.read_info()?;
        let (width, height, color_type, bit_depth) = {
            let info = reader.info();
            (info.width, info.height, info.color_type, info.bit_depth)
        };
        let maximum_size = self.thumbnail_size.pixel_size();
        if width > maximum_size || height > maximum_size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "thumbnail output {width}x{height} exceeds cache size {maximum_size}x{maximum_size}"
                ),
            )
            .into());
        }

        let mut image_data = vec![
            0;
            reader
                .output_buffer_size()
                .ok_or("the required thumbnail buffer is too large")?
        ];
        let output = reader.next_frame(&mut image_data)?;
        image_data.truncate(output.buffer_size());

        let mut cache_temp = self.cache_temp_file(&self.thumbnail_dir)?;
        {
            let mut output = BufWriter::new(cache_temp.as_file_mut());
            let mut encoder = png::Encoder::new(&mut output, width, height);
            encoder.set_color(color_type);
            encoder.set_depth(bit_depth);
            self.add_thumbnail_text_metadata(&mut encoder)?;
            encoder.write_header()?.write_image_data(&image_data)?;
            output.flush()?;
        }

        self.publish(cache_temp, &self.thumbnail_path)?;
        Ok(())
    }

    pub fn update_with_image(&self, image: DynamicImage) -> Result<&Path, Box<dyn Error>> {
        let mut temp_file = self.cache_temp_file(&self.thumbnail_dir)?;
        {
            let image = image
                .thumbnail(
                    self.thumbnail_size.pixel_size(),
                    self.thumbnail_size.pixel_size(),
                )
                .into_rgba8();
            let mut output = BufWriter::new(temp_file.as_file_mut());
            let mut encoder = png::Encoder::new(&mut output, image.width(), image.height());
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            self.add_thumbnail_text_metadata(&mut encoder)?;
            encoder.write_header()?.write_image_data(image.as_raw())?;
            output.flush()?;
        }

        self.publish(temp_file, &self.thumbnail_path)?;
        Ok(&self.thumbnail_path)
    }

    pub fn create_fail_marker(&self) -> Result<(), Box<dyn Error>> {
        let fail_dir = self
            .thumbnail_fail_marker_path
            .parent()
            .ok_or("thumbnail fail marker has no parent directory")?;
        Self::ensure_private_directory(fail_dir)?;
        let mut temp_file = self.cache_temp_file(fail_dir)?;
        {
            let mut output = BufWriter::new(temp_file.as_file_mut());
            let mut encoder = png::Encoder::new(&mut output, 1, 1);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::One);
            self.add_thumbnail_text_metadata(&mut encoder)?;
            encoder.write_header()?.write_image_data(&[0])?;
            output.flush()?;
        }
        self.publish(temp_file, &self.thumbnail_fail_marker_path)?;
        Ok(())
    }

    fn add_thumbnail_text_metadata<W: Write>(
        &self,
        encoder: &mut png::Encoder<'_, W>,
    ) -> Result<(), Box<dyn Error>> {
        let metadata = std::fs::metadata(&self.file_path)?;
        let mtime = metadata
            .modified()?
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        for (keyword, text) in [
            ("Software", "COSMIC Files".to_string()),
            ("Thumb::URI", self.file_uri.clone()),
            ("Thumb::Size", metadata.len().to_string()),
            ("Thumb::MTime", mtime.to_string()),
        ] {
            encoder.add_text_chunk(keyword.to_string(), text)?;
        }
        Ok(())
    }

    fn maximum_decoded_bytes(&self) -> usize {
        let edge = self.thumbnail_size.pixel_size() as usize;
        // Allow the bucket's largest 16-bit RGBA frame.
        edge.saturating_mul(edge).saturating_mul(8)
    }

    fn cache_temp_file(&self, directory: &Path) -> io::Result<NamedTempFile> {
        tempfile::Builder::new()
            .prefix(".cosmic-files-")
            .tempfile_in(directory)
    }

    fn publish(&self, temp_file: NamedTempFile, destination: &Path) -> Result<(), Box<dyn Error>> {
        #[cfg(unix)]
        fs::set_permissions(temp_file.path(), fs::Permissions::from_mode(0o600))?;
        temp_file.persist(destination)?;
        Ok(())
    }

    fn ensure_private_directory(directory: &Path) -> io::Result<()> {
        fs::create_dir_all(directory)?;
        #[cfg(unix)]
        {
            let permissions = fs::metadata(directory)?.permissions();
            if permissions.mode() & 0o777 != 0o700 {
                fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
            }
        }
        Ok(())
    }

    fn is_thumbnail_valid(&self, thumbnail_path: &Path) -> bool {
        let thumbnail_file = match File::open(thumbnail_path) {
            Ok(file) => file,
            Err(_) => return false,
        };
        let decoder = png::Decoder::new(BufReader::new(thumbnail_file));
        let reader = match decoder.read_info() {
            Ok(reader) => reader,
            Err(err) => {
                log::warn!(
                    "failed to decode {} as PNG: {}",
                    thumbnail_path.display(),
                    err
                );
                return false;
            }
        };

        let texts = &reader.info().uncompressed_latin1_text;

        // Thumb::URI is required and must match.
        let thumb_uri = texts
            .iter()
            .find(|&text| text.keyword == "Thumb::URI")
            .map(|t| &t.text);
        if let Some(thumb_uri) = thumb_uri {
            if *thumb_uri != self.file_uri {
                return false;
            }
        } else {
            return false;
        }

        let metadata = match std::fs::metadata(&self.file_path) {
            Ok(m) => m,
            Err(err) => {
                log::warn!(
                    "failed to get metatdata of {}: {}",
                    self.file_path.display(),
                    err
                );
                return false;
            }
        };

        // Thumb::MTime is required and must match.
        let thumb_mtime = texts
            .iter()
            .find(|&text| text.keyword == "Thumb::MTime")
            .map(|t| &t.text);
        if let Some(thumb_mtime) = thumb_mtime {
            let modified = match metadata.modified() {
                Ok(m) => m,
                Err(err) => {
                    log::warn!(
                        "failed to get modified from metatdata of {}, {}",
                        self.file_path.display(),
                        err
                    );
                    return false;
                }
            };
            let mtime = modified
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                .to_string();
            if *thumb_mtime != mtime {
                return false;
            }
        } else {
            return false;
        }

        // Thumb::Size isn't required, but it should be verified if present.
        let thumb_size = texts
            .iter()
            .find(|&text| text.keyword == "Thumb::Size")
            .map(|t| &t.text);
        if let Some(thumb_size) = thumb_size {
            let size = metadata.len();
            if *thumb_size != size.to_string() {
                return false;
            }
        }

        true
    }
}

fn thumbnail_uri(path: &Path) -> io::Result<String> {
    thumbnail_uri_from_path(&fs::canonicalize(path)?)
}

fn thumbnail_uri_from_path(absolute_path: &Path) -> io::Result<String> {
    let url = Url::from_file_path(absolute_path).map_err(|()| {
        io::Error::other(format!(
            "failed to create URI for thumbnail_file: {}",
            absolute_path.display()
        ))
    })?;
    // Technically square brackets don't need to be percent encoded,
    // and they aren't by the url crate, but the thumbnailer used by
    // Gnome Files does. In order to share thumbnails and not get duplicates
    // we should do the same.
    let url = url.as_str().replace('[', "%5B").replace(']', "%5D");
    Ok(url)
}

fn thumbnail_uri_lossy(path: &Path) -> Option<String> {
    if let Ok(uri) = thumbnail_uri(path) {
        return Some(uri);
    }

    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    thumbnail_uri_from_path(&absolute).ok()
}

fn thumbnail_cache_filename(file_uri: &str) -> String {
    let hash = Md5::digest(file_uri);
    format!("{hash:x}.png")
}

fn fail_marker_path(cache_base_dir: &Path, cache_filename: &str) -> PathBuf {
    cache_base_dir
        .join("fail")
        .join(format!("cosmic-files-{}", env!("CARGO_PKG_VERSION")))
        .join(cache_filename)
}

fn remove_file_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

fn remove_cached_entries(cache_base_dir: &Path, cache_filename: &str) -> io::Result<()> {
    for size in ThumbnailSize::ALL {
        remove_file_if_exists(
            &cache_base_dir
                .join(size.subdirectory_name())
                .join(cache_filename),
        )?;
    }
    remove_file_if_exists(&fail_marker_path(cache_base_dir, cache_filename))
}

fn cacher_for(
    file_path: &Path,
    file_uri: &str,
    cache_base_dir: &Path,
    cache_filename: &str,
    size: ThumbnailSize,
) -> ThumbnailCacher {
    let thumbnail_dir = cache_base_dir.join(size.subdirectory_name());
    ThumbnailCacher {
        file_path: file_path.to_path_buf(),
        file_uri: file_uri.to_string(),
        thumbnail_path: thumbnail_dir.join(cache_filename),
        thumbnail_dir,
        thumbnail_size: size,
        thumbnail_fail_marker_path: fail_marker_path(cache_base_dir, cache_filename),
    }
}

fn valid_entries(
    file_path: &Path,
    file_uri: &str,
    cache_base_dir: &Path,
    cache_filename: &str,
) -> (Vec<ThumbnailSize>, bool) {
    let mut valid_sizes = Vec::new();
    for size in ThumbnailSize::ALL {
        let cacher = cacher_for(file_path, file_uri, cache_base_dir, cache_filename, size);
        if cacher.is_thumbnail_valid(&cacher.thumbnail_path) {
            valid_sizes.push(size);
        }
    }

    let fail_cacher = cacher_for(
        file_path,
        file_uri,
        cache_base_dir,
        cache_filename,
        ThumbnailSize::Normal,
    );
    let valid_fail_marker = fail_cacher.is_thumbnail_valid(&fail_cacher.thumbnail_fail_marker_path);

    (valid_sizes, valid_fail_marker)
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum ThumbnailSize {
    Normal = 128,
    Large = 256,
    XLarge = 512,
    XXLarge = 1024,
}

impl ThumbnailSize {
    pub const ALL: [Self; 4] = [Self::Normal, Self::Large, Self::XLarge, Self::XXLarge];

    pub fn from_pixel_size(pixel_size: u32) -> Self {
        if pixel_size <= Self::Normal.pixel_size() {
            Self::Normal
        } else if pixel_size <= Self::Large.pixel_size() {
            Self::Large
        } else if pixel_size <= Self::XLarge.pixel_size() {
            Self::XLarge
        } else {
            Self::XXLarge
        }
    }

    pub const fn pixel_size(self) -> u32 {
        self as u32
    }

    pub const fn subdirectory_name(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Large => "large",
            Self::XLarge => "x-large",
            Self::XXLarge => "xx-large",
        }
    }
}

pub enum CachedThumbnail {
    /// The cached thumbnail is valid and should be used with size if known.
    Valid((PathBuf, Option<ThumbnailSize>)),
    /// The cached thumbnail doesn't exist or it's invalid and
    /// needs to be recreated with the pixel size.
    RequiresUpdate(ThumbnailSize),
    // The cached thumbnail is in a failed state.
    // This means it failed to create by cosmic-files in the past
    // and shouldn't be tried again.
    Failed,
}

static THUMBNAIL_CACHE_BASE_DIR: LazyLock<Option<PathBuf>> = LazyLock::new(|| {
    if let Some(cache_dir) = dirs::cache_dir() {
        return Some(cache_dir.join("thumbnails"));
    }

    log::warn!("failed to get thumbnail cache directory, thumbnails will not be cached");

    None
});

pub struct ThumbnailRelocation {
    from: PathBuf,
    cache_base_dir: PathBuf,
    cache_filename: String,
    valid_sizes: Vec<ThumbnailSize>,
    valid_fail_marker: bool,
}

impl ThumbnailRelocation {
    pub fn new(from: &Path) -> Option<Self> {
        let cache_base_dir = THUMBNAIL_CACHE_BASE_DIR.as_ref()?.clone();
        let file_uri = thumbnail_uri(from).ok()?;
        Self::new_in(from, from, file_uri, cache_base_dir)
    }

    fn new_in(
        key_path: &Path,
        metadata_path: &Path,
        file_uri: String,
        cache_base_dir: PathBuf,
    ) -> Option<Self> {
        if !metadata_path
            .symlink_metadata()
            .is_ok_and(|m| !m.file_type().is_symlink())
        {
            return None;
        }

        let cache_filename = thumbnail_cache_filename(&file_uri);
        let (valid_sizes, valid_fail_marker) =
            valid_entries(metadata_path, &file_uri, &cache_base_dir, &cache_filename);

        Some(Self {
            from: key_path.to_path_buf(),
            cache_filename,
            cache_base_dir,
            valid_sizes,
            valid_fail_marker,
        })
    }

    pub fn copy(&self, to: &Path) -> bool {
        self.copy_entries(to)
    }

    pub fn relocate(&self, to: &Path) -> bool {
        let copied = self.copy_entries(to);
        if !self.from.exists() {
            self.remove_cached();
        }
        copied
    }

    pub fn remove_cached(&self) {
        if let Err(err) = remove_cached_entries(&self.cache_base_dir, &self.cache_filename) {
            log::warn!(
                "failed to remove cached thumbnails for {}: {}",
                self.from.display(),
                err
            );
        }
    }

    fn copy_entries(&self, to: &Path) -> bool {
        let Ok(to_uri) = thumbnail_uri(to) else {
            return false;
        };
        let to_filename = thumbnail_cache_filename(&to_uri);
        let mut copied = false;

        for &size in &self.valid_sizes {
            let source = self
                .cache_base_dir
                .join(size.subdirectory_name())
                .join(&self.cache_filename);
            let cacher = cacher_for(to, &to_uri, &self.cache_base_dir, &to_filename, size);
            if let Err(err) = ThumbnailCacher::ensure_private_directory(&cacher.thumbnail_dir) {
                log::warn!(
                    "failed to prepare thumbnail directory {}: {}",
                    cacher.thumbnail_dir.display(),
                    err
                );
                continue;
            }
            match cacher.update_from_file(&source) {
                Ok(()) => copied = true,
                Err(err) => {
                    log::warn!(
                        "failed to relocate cached thumbnail {} to {}: {}",
                        source.display(),
                        to.display(),
                        err
                    );
                }
            }
        }

        if self.valid_fail_marker {
            let cacher = cacher_for(
                to,
                &to_uri,
                &self.cache_base_dir,
                &to_filename,
                ThumbnailSize::Normal,
            );
            if let Err(err) = cacher.create_fail_marker() {
                log::warn!(
                    "failed to relocate thumbnail fail marker to {}: {}",
                    to.display(),
                    err
                );
            } else {
                copied = true;
            }
        }

        copied
    }
}

pub fn relocate_thumbnails_tree(old_root: &Path, new_root: &Path) {
    let Some(cache_base_dir) = THUMBNAIL_CACHE_BASE_DIR.as_ref() else {
        return;
    };
    relocate_thumbnails_tree_in(old_root, new_root, cache_base_dir);
}

fn relocate_thumbnails_tree_in(old_root: &Path, new_root: &Path, cache_base_dir: &Path) {
    for entry in WalkDir::new(new_root).into_iter().filter_map(Result::ok) {
        let file_type = entry.file_type();
        if file_type.is_dir() || file_type.is_symlink() {
            continue;
        }

        let Ok(relative) = entry.path().strip_prefix(new_root) else {
            continue;
        };
        let old_path = old_root.join(relative);
        let file_uri = thumbnail_uri(&old_path).or_else(|_| thumbnail_uri_from_path(&old_path));
        let Ok(file_uri) = file_uri else {
            continue;
        };

        if let Some(relocation) = ThumbnailRelocation::new_in(
            &old_path,
            entry.path(),
            file_uri,
            cache_base_dir.to_path_buf(),
        ) {
            relocation.relocate(entry.path());
        }
    }
}

pub fn remove_thumbnails(path: &Path) {
    if path
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        return;
    }

    let Some(file_uri) = thumbnail_uri_lossy(path) else {
        return;
    };
    remove_thumbnails_for_uri(path, &file_uri);
}

pub fn remove_thumbnails_recursive(path: &Path) {
    let Ok(metadata) = path.symlink_metadata() else {
        return;
    };
    if metadata.file_type().is_symlink() {
        return;
    }

    if metadata.is_dir() {
        let root = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        remove_thumbnails_for_canonical_path(&root);
        for entry in WalkDir::new(&root).into_iter().filter_map(Result::ok) {
            if entry.file_type().is_symlink() {
                continue;
            }
            remove_thumbnails_for_canonical_path(entry.path());
        }
    } else {
        remove_thumbnails(path);
    }
}

fn remove_thumbnails_for_canonical_path(path: &Path) {
    let Ok(file_uri) = thumbnail_uri_from_path(path) else {
        return;
    };
    remove_thumbnails_for_uri(path, &file_uri);
}

fn remove_thumbnails_for_uri(path: &Path, file_uri: &str) {
    let Some(cache_base_dir) = THUMBNAIL_CACHE_BASE_DIR.as_ref() else {
        return;
    };
    let cache_filename = thumbnail_cache_filename(file_uri);
    if let Err(err) = remove_cached_entries(cache_base_dir, &cache_filename) {
        log::warn!(
            "failed to remove cached thumbnails for {}: {}",
            path.display(),
            err
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, RgbaImage};
    use tempfile::TempDir;

    fn test_cacher(root: &TempDir, size: ThumbnailSize) -> ThumbnailCacher {
        let source = root.path().join("source.pdf");
        fs::write(&source, b"source data").unwrap();
        let thumbnail_dir = root.path().join(size.subdirectory_name());
        ThumbnailCacher::ensure_private_directory(&thumbnail_dir).unwrap();

        ThumbnailCacher {
            file_path: source,
            file_uri: "file:///source.pdf".to_string(),
            thumbnail_path: thumbnail_dir.join("thumbnail.png"),
            thumbnail_dir,
            thumbnail_size: size,
            thumbnail_fail_marker_path: fail_marker_path(root.path(), "thumbnail.png"),
        }
    }

    fn external_png(root: &TempDir, width: u32, height: u32) -> NamedTempFile {
        let mut file = tempfile::Builder::new()
            .prefix("external-")
            .tempfile_in(root.path())
            .unwrap();
        {
            let mut encoder = png::Encoder::new(file.as_file_mut(), width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&vec![0x7f; width as usize * height as usize * 4])
                .unwrap();
        }
        file
    }

    #[test]
    fn external_thumbnail_is_staged_with_freedesktop_metadata() {
        let root = TempDir::new().unwrap();
        let cacher = test_cacher(&root, ThumbnailSize::Normal);
        let external = external_png(&root, 64, 32);

        let cached_path = cacher.update_with_temp_file(external).unwrap();

        assert_eq!(cached_path, cacher.thumbnail_path);
        assert!(cacher.is_thumbnail_valid(cached_path));
        let reader = png::Decoder::new(BufReader::new(File::open(cached_path).unwrap()))
            .read_info()
            .unwrap();
        let texts = &reader.info().uncompressed_latin1_text;
        assert!(
            texts
                .iter()
                .any(|text| { text.keyword == "Software" && text.text == "COSMIC Files" })
        );
        assert!(
            texts
                .iter()
                .any(|text| { text.keyword == "Thumb::URI" && text.text == cacher.file_uri })
        );
    }

    #[test]
    fn oversized_external_thumbnail_is_rejected() {
        let root = TempDir::new().unwrap();
        let cacher = test_cacher(&root, ThumbnailSize::Normal);
        let external = external_png(&root, 129, 1);

        assert!(cacher.update_with_temp_file(external).is_err());
        assert!(!cacher.thumbnail_path.exists());
    }

    #[test]
    fn image_and_failure_updates_publish_valid_cache_entries() {
        let root = TempDir::new().unwrap();
        let cacher = test_cacher(&root, ThumbnailSize::Normal);
        let image = DynamicImage::ImageRgba8(RgbaImage::new(16, 8));

        let cached_path = cacher.update_with_image(image).unwrap();
        assert!(cacher.is_thumbnail_valid(cached_path));

        cacher.create_fail_marker().unwrap();
        assert!(cacher.is_thumbnail_valid(&cacher.thumbnail_fail_marker_path));
    }

    #[test]
    fn cached_thumbnail_can_be_rewritten_for_a_new_path() {
        let root = TempDir::new().unwrap();
        let source = test_cacher(&root, ThumbnailSize::Normal);
        let cached_path = source
            .update_with_image(DynamicImage::ImageRgba8(RgbaImage::new(16, 8)))
            .unwrap()
            .to_path_buf();

        let renamed = root.path().join("renamed.pdf");
        fs::write(&renamed, b"source data").unwrap();
        let thumbnail_dir = root.path().join(ThumbnailSize::Normal.subdirectory_name());
        let dest = ThumbnailCacher {
            file_path: renamed,
            file_uri: "file:///renamed.pdf".to_string(),
            thumbnail_path: thumbnail_dir.join("renamed.png"),
            thumbnail_dir,
            thumbnail_size: ThumbnailSize::Normal,
            thumbnail_fail_marker_path: root.path().join("fail/renamed.png"),
        };

        dest.update_from_file(&cached_path).unwrap();

        assert!(dest.is_thumbnail_valid(&dest.thumbnail_path));
    }

    #[test]
    fn relocation_ignores_invalid_cache_entries() {
        let root = TempDir::new().unwrap();
        let cacher = test_cacher(&root, ThumbnailSize::Normal);
        cacher
            .update_with_image(DynamicImage::ImageRgba8(RgbaImage::new(16, 8)))
            .unwrap();

        let (sizes, failed) = valid_entries(
            &cacher.file_path,
            &cacher.file_uri,
            root.path(),
            "thumbnail.png",
        );
        assert_eq!(sizes, vec![ThumbnailSize::Normal]);
        assert!(!failed);

        // Changing the file invalidates the entry, so it is not relocated.
        fs::write(&cacher.file_path, b"modified source data").unwrap();
        let (sizes, failed) = valid_entries(
            &cacher.file_path,
            &cacher.file_uri,
            root.path(),
            "thumbnail.png",
        );
        assert!(sizes.is_empty());
        assert!(!failed);

        // Fail markers are only relocated when still valid.
        cacher.create_fail_marker().unwrap();
        let (_, failed) = valid_entries(
            &cacher.file_path,
            &cacher.file_uri,
            root.path(),
            "thumbnail.png",
        );
        assert!(failed);
    }

    #[test]
    fn moved_path_relocates_valid_entries_against_new_metadata() {
        let root = TempDir::new().unwrap();
        let file = root.path().join("photo.jpg");
        fs::write(&file, b"image data").unwrap();
        let cache_base = root.path().join("cache");

        let old_uri = thumbnail_uri(&file).unwrap();
        let old_filename = thumbnail_cache_filename(&old_uri);
        let old_cacher = cacher_for(
            &file,
            &old_uri,
            &cache_base,
            &old_filename,
            ThumbnailSize::Normal,
        );
        ThumbnailCacher::ensure_private_directory(&old_cacher.thumbnail_dir).unwrap();
        old_cacher
            .update_with_image(DynamicImage::ImageRgba8(RgbaImage::new(4, 4)))
            .unwrap();

        let new_file = root.path().join("renamed.jpg");
        fs::rename(&file, &new_file).unwrap();

        let relocation = ThumbnailRelocation::new_in(&file, &new_file, old_uri, cache_base.clone())
            .expect("the valid entry should be captured");
        assert!(relocation.relocate(&new_file));

        let new_uri = thumbnail_uri(&new_file).unwrap();
        let new_filename = thumbnail_cache_filename(&new_uri);
        let new_cacher = cacher_for(
            &new_file,
            &new_uri,
            &cache_base,
            &new_filename,
            ThumbnailSize::Normal,
        );
        assert!(new_cacher.is_thumbnail_valid(&new_cacher.thumbnail_path));
        assert!(!cache_base.join("normal").join(&old_filename).exists());
    }

    #[test]
    fn directory_rename_relocates_descendant_thumbnails() {
        let root = TempDir::new().unwrap();
        let old_dir = root.path().join("old");
        fs::create_dir_all(old_dir.join("nested")).unwrap();
        let cache_base = root.path().join("cache");
        let relative_paths = ["photo.jpg", "nested/deep.jpg"];

        for relative in relative_paths {
            let file = old_dir.join(relative);
            fs::write(&file, b"image data").unwrap();
            let uri = thumbnail_uri(&file).unwrap();
            let filename = thumbnail_cache_filename(&uri);
            let cacher = cacher_for(&file, &uri, &cache_base, &filename, ThumbnailSize::Normal);
            ThumbnailCacher::ensure_private_directory(&cacher.thumbnail_dir).unwrap();
            cacher
                .update_with_image(DynamicImage::ImageRgba8(RgbaImage::new(4, 4)))
                .unwrap();
        }

        let old_root = fs::canonicalize(&old_dir).unwrap();
        let new_dir = root.path().join("new");
        fs::rename(&old_dir, &new_dir).unwrap();

        relocate_thumbnails_tree_in(&old_root, &new_dir, &cache_base);

        for relative in relative_paths {
            let new_file = new_dir.join(relative);
            let new_uri = thumbnail_uri(&new_file).unwrap();
            let new_filename = thumbnail_cache_filename(&new_uri);
            let new_cacher = cacher_for(
                &new_file,
                &new_uri,
                &cache_base,
                &new_filename,
                ThumbnailSize::Normal,
            );
            assert!(
                new_cacher.is_thumbnail_valid(&new_cacher.thumbnail_path),
                "missing relocated thumbnail for {relative}"
            );

            let old_file = old_root.join(relative);
            let old_uri = thumbnail_uri_from_path(&old_file).unwrap();
            let old_filename = thumbnail_cache_filename(&old_uri);
            assert!(
                !cache_base.join("normal").join(&old_filename).exists(),
                "old thumbnail not removed for {relative}"
            );
        }
    }

    #[test]
    fn remove_cached_entries_delete_thumbnail_and_fail_marker() {
        let root = TempDir::new().unwrap();
        let cacher = test_cacher(&root, ThumbnailSize::Normal);
        cacher
            .update_with_image(DynamicImage::ImageRgba8(RgbaImage::new(16, 8)))
            .unwrap();
        cacher.create_fail_marker().unwrap();

        remove_cached_entries(root.path(), "thumbnail.png").unwrap();

        assert!(!cacher.thumbnail_path.exists());
        assert!(!cacher.thumbnail_fail_marker_path.exists());
    }

    #[test]
    fn item_thumbnail_cache_lookup_never_generates() {
        use crate::tab::ItemThumbnail;
        use mime_guess::Mime;

        let root = TempDir::new().unwrap();
        let cache_base = root.path().join("cache");
        let mime: Mime = "image/jpeg".parse().unwrap();

        let cached_file = root.path().join("cached.jpg");
        fs::write(&cached_file, b"data").unwrap();
        let cached_uri = thumbnail_uri(&cached_file).unwrap();
        let cached_filename = thumbnail_cache_filename(&cached_uri);
        let cached_cacher = cacher_for(
            &cached_file,
            &cached_uri,
            &cache_base,
            &cached_filename,
            ThumbnailSize::Normal,
        );
        ThumbnailCacher::ensure_private_directory(&cached_cacher.thumbnail_dir).unwrap();
        cached_cacher
            .update_with_image(DynamicImage::ImageRgba8(RgbaImage::new(4, 4)))
            .unwrap();
        assert!(ItemThumbnail::from_cache(&cached_cacher, &mime, None).is_some());

        // A file with no cached thumbnail is left to be generated.
        let uncached_file = root.path().join("uncached.jpg");
        fs::write(&uncached_file, b"data").unwrap();
        let uncached_uri = thumbnail_uri(&uncached_file).unwrap();
        let uncached_filename = thumbnail_cache_filename(&uncached_uri);
        let uncached_cacher = cacher_for(
            &uncached_file,
            &uncached_uri,
            &cache_base,
            &uncached_filename,
            ThumbnailSize::Normal,
        );
        assert!(ItemThumbnail::from_cache(&uncached_cacher, &mime, None).is_none());
    }
}
