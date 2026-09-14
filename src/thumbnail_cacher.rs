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
        let thumbnail_fail_marker_path = cache_base_dir
            .join("fail")
            .join(format!("cosmic-files-{}", env!("CARGO_PKG_VERSION")))
            .join(&thumbnail_filename);

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
        let file = File::open(temp_file.path())?;
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
        Ok(&self.thumbnail_path)
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
    let absolute_path = fs::canonicalize(path)?;
    let url = Url::from_file_path(&absolute_path).map_err(|()| {
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

fn thumbnail_cache_filename(file_uri: &str) -> String {
    let hash = Md5::digest(file_uri);
    format!("{hash:x}.png")
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
            thumbnail_fail_marker_path: root.path().join("fail/thumbnail.png"),
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
}
