// SPDX-License-Identifier: GPL-3.0-only

use quick_xml::DeError;
use quick_xml::de::from_str;
use quick_xml::writer::Writer;
use serde::Deserialize;
use std::collections::HashSet;
use std::fs;
use std::io::{self, Cursor};
use std::path::{Path, PathBuf};
use std::string::FromUtf8Error;
use std::time::SystemTime;
use thiserror::Error;
use url::Url;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename = "xbel", rename_all = "kebab-case")]
pub struct RecentlyUsed {
    #[allow(dead_code)]
    #[serde(rename = "@xmlns:bookmark")]
    xmlns_bookmark: String,
    #[allow(dead_code)]
    #[serde(rename = "@xmlns:mime")]
    xmlns_mime: String,
    #[serde(rename = "bookmark", default)]
    pub bookmarks: Vec<Bookmark>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Bookmark {
    #[serde(rename = "@href")]
    pub href: String,
    #[serde(rename = "@added")]
    added: String,
    #[serde(rename = "@modified")]
    pub modified: String,
    #[serde(rename = "@visited")]
    pub visited: String,
    #[serde(rename = "info")]
    info: Option<Info>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Info {
    #[serde(rename = "metadata")]
    metadata: Metadata,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Metadata {
    #[serde(rename = "@owner")]
    owner: String,
    #[serde(rename = "mime-type")]
    mime_type: Option<MimeType>,
    #[serde(rename = "applications")]
    applications: Applications,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct MimeType {
    #[serde(rename = "@type")]
    mime_type: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Applications {
    #[serde(rename = "application")]
    applications: Vec<Application>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Application {
    #[serde(rename = "@name")]
    name: String,
    #[serde(rename = "@exec")]
    exec: String,
    #[serde(rename = "@modified")]
    modified: String,
    #[serde(rename = "@count")]
    count: u32,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("~/.local/share/recently-used.xbel: file does not exist")]
    DoesNotExist,
    #[error("~/.local/share/recently-used.xbel: could not deserialize")]
    Deserialization(#[source] DeError),
    #[error("could not read recently used files")]
    Read(#[source] io::Error),
    #[error("could not read metadata from path")]
    Metadata(#[source] io::Error),
    #[error("could not serialize recently used files")]
    Serialization(#[source] io::Error),
    #[error("could not convert file time to an XBEL timestamp")]
    Timestamp,
    #[error("could not generate href from path")]
    Path,
    #[error("could not update recently used files")]
    Update,
    #[error("serialized XBEL was not UTF-8")]
    Utf8(#[from] FromUtf8Error),
}

/// Returns the location of the user's freedesktop recent-files list.
pub fn dir() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".local/share/recently-used.xbel"))
}

/// Reads the user's recent-files list.
pub fn parse_file() -> Result<RecentlyUsed, Error> {
    let path = dir().ok_or(Error::DoesNotExist)?;
    let content = fs::read_to_string(path).map_err(Error::Read)?;
    from_str(&content).map_err(Error::Deserialization)
}

/// Clears the bookmarks while keeping the recent-files list in XBEL format.
pub fn clear_recently_used() -> Result<(), Error> {
    let mut recently_used = parse_file()?;
    recently_used.bookmarks.clear();
    write_file(&recently_used)
}

/// Adds or refreshes a file bookmark and the application's usage count.
pub fn update_recently_used(
    element_path: &Path,
    app_name: String,
    exec: String,
    owner: Option<String>,
) -> Result<(), Error> {
    let mut recently_used = parse_file()?;
    let href = path_to_href(element_path).ok_or(Error::Path)?;
    let metadata = element_path.metadata().map_err(Error::Metadata)?;
    let added = timestamp(metadata.created().map_err(Error::Metadata)?)?;
    let modified = timestamp(metadata.modified().map_err(Error::Metadata)?)?;
    let visited = timestamp(metadata.accessed().map_err(Error::Metadata)?)?;

    if let Some(bookmark) = recently_used
        .bookmarks
        .iter_mut()
        .find(|item| item.href == href)
    {
        bookmark.added = added;
        bookmark.modified = modified.clone();
        bookmark.visited = visited;

        if let Some(info) = bookmark.info.as_mut()
            && let Some(application) = info
                .metadata
                .applications
                .applications
                .iter_mut()
                .find(|item| item.name == app_name)
        {
            application.count += 1;
            application.modified = modified.clone();
        } else if let Some(info) = bookmark.info.as_mut() {
            info.metadata.applications.applications.push(Application {
                name: app_name,
                exec,
                modified: modified.clone(),
                count: 1,
            });
        }
    } else {
        let mime = crate::mime_icon::mime_for_path(element_path, None, false);
        let mime_type = (mime != mime::APPLICATION_OCTET_STREAM).then(|| MimeType {
            mime_type: mime.essence_str().to_owned(),
        });
        let application = Application {
            name: app_name,
            exec,
            modified: modified.clone(),
            count: 1,
        };
        let info = Info {
            metadata: Metadata {
                owner: owner.unwrap_or_else(|| "http://freedesktop.org".to_owned()),
                mime_type,
                applications: Applications {
                    applications: vec![application],
                },
            },
        };
        recently_used.bookmarks.push(Bookmark {
            href,
            added,
            modified,
            visited,
            info: Some(info),
        });
    }

    write_file(&recently_used)
}

/// Removes bookmarks for the given paths.
pub fn remove_recently_used(element_paths: &[&Path]) -> Result<(), Error> {
    let mut recently_used = parse_file()?;
    let mut hrefs = HashSet::with_capacity(element_paths.len());
    for path in element_paths {
        hrefs.insert(path_to_href(path).ok_or(Error::Path)?);
    }
    recently_used
        .bookmarks
        .retain(|bookmark| !hrefs.contains(&bookmark.href));
    write_file(&recently_used)
}

fn timestamp(time: SystemTime) -> Result<String, Error> {
    jiff::Timestamp::try_from(time)
        .map(|timestamp| timestamp.to_string())
        .map_err(|_| Error::Timestamp)
}

fn path_to_href(path: &Path) -> Option<String> {
    let path = path.to_str()?;
    Url::from_file_path(path).ok().map(Into::into)
}

fn write_file(recently_used: &RecentlyUsed) -> Result<(), Error> {
    let serialized = serialize(recently_used)?;
    let path = dir().ok_or(Error::DoesNotExist)?;
    fs::write(path, serialized).map_err(|_| Error::Update)
}

fn serialize(recently_used: &RecentlyUsed) -> Result<String, Error> {
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    writer
        .create_element("xbel")
        .with_attributes([
            ("version", "1.0"),
            (
                "xmlns:bookmark",
                "http://www.freedesktop.org/standards/desktop-bookmarks",
            ),
            (
                "xmlns:mime",
                "http://www.freedesktop.org/standards/shared-mime-info",
            ),
        ])
        .write_inner_content(|writer| {
            for bookmark in &recently_used.bookmarks {
                writer
                    .create_element("bookmark")
                    .with_attributes([
                        ("href", bookmark.href.as_str()),
                        ("added", bookmark.added.as_str()),
                        ("modified", bookmark.modified.as_str()),
                        ("visited", bookmark.visited.as_str()),
                    ])
                    .write_inner_content(|writer| {
                        if let Some(info) = &bookmark.info {
                            writer
                                .create_element("info")
                                .write_inner_content(|writer| {
                                    writer
                                        .create_element("metadata")
                                        .with_attribute(("owner", info.metadata.owner.as_str()))
                                        .write_inner_content(|writer| {
                                            if let Some(mime_type) = &info.metadata.mime_type {
                                                writer
                                                    .create_element("mime:mime-type")
                                                    .with_attribute((
                                                        "type",
                                                        mime_type.mime_type.as_str(),
                                                    ))
                                                    .write_empty()?;
                                            }
                                            writer
                                                .create_element("bookmark:applications")
                                                .write_inner_content(|writer| {
                                                    for application in
                                                        &info.metadata.applications.applications
                                                    {
                                                        let count = application.count.to_string();
                                                        writer
                                                            .create_element("bookmark:application")
                                                            .with_attributes([
                                                                ("name", application.name.as_str()),
                                                                ("exec", application.exec.as_str()),
                                                                (
                                                                    "modified",
                                                                    application.modified.as_str(),
                                                                ),
                                                                ("count", count.as_str()),
                                                            ])
                                                            .write_empty()?;
                                                    }
                                                    Ok(())
                                                })?;
                                            Ok(())
                                        })?;
                                    Ok(())
                                })?;
                        }
                        Ok(())
                    })?;
            }
            Ok(())
        })
        .map_err(Error::Serialization)?;

    let xml = String::from_utf8(writer.into_inner().into_inner())?;
    Ok(format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>{xml}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_writes_freedesktop_xbel() {
        let source = r#"<?xml version="1.0" encoding="UTF-8"?>
<xbel version="1.0" xmlns:bookmark="http://www.freedesktop.org/standards/desktop-bookmarks" xmlns:mime="http://www.freedesktop.org/standards/shared-mime-info">
  <bookmark href="file:///tmp/report%20%26%20draft.txt" added="2026-10-04T00:00:00Z" modified="2026-10-04T00:01:00Z" visited="2026-10-04T00:02:00Z">
    <info><metadata owner="http://freedesktop.org"><mime:mime-type type="text/plain"/><bookmark:applications><bookmark:application name="demo&amp;app" exec="demo %F" modified="2026-10-04T00:01:00Z" count="2"/></bookmark:applications></metadata></info>
  </bookmark>
</xbel>"#;

        let parsed: RecentlyUsed = from_str(source).expect("valid XBEL should parse");
        assert_eq!(parsed.bookmarks.len(), 1);
        assert_eq!(
            parsed.bookmarks[0].href,
            "file:///tmp/report%20%26%20draft.txt"
        );
        assert_eq!(
            parsed.bookmarks[0]
                .info
                .as_ref()
                .unwrap()
                .metadata
                .applications
                .applications[0]
                .name,
            "demo&app"
        );

        let encoded = serialize(&parsed).expect("bookmark should serialize");
        assert!(encoded.contains("name=\"demo&amp;app\""));
        let reparsed: RecentlyUsed = from_str(&encoded).expect("written XBEL should parse");
        assert_eq!(reparsed.bookmarks.len(), 1);
        assert_eq!(reparsed.bookmarks[0].href, parsed.bookmarks[0].href);
        assert_eq!(
            reparsed.bookmarks[0]
                .info
                .as_ref()
                .unwrap()
                .metadata
                .applications
                .applications[0]
                .count,
            2
        );
    }
}
