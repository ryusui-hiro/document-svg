use std::collections::HashMap;
use std::fs::File;
use std::io::{Cursor, Read, Seek};
use std::path::{Component, Path, PathBuf};

use quick_xml::Reader;
use quick_xml::events::Event;
use zip::ZipArchive;

use crate::error::{Error, Result};
use crate::ooxml::{attribute, local_name};

/// ZIP central directories are metadata that is allocated before any part is
/// read. Keep a package-wide cap so an archive containing millions of tiny
/// entries cannot exhaust memory even when each individual entry is small.
pub(crate) const MAX_ZIP_PACKAGE_ENTRIES: usize = 100_000;

pub(crate) struct ZipPackage<R: Read + Seek> {
    archive: ZipArchive<R>,
    max_entry_bytes: u64,
}

/// Compound File Binary signature. Office wraps a password-protected document
/// in one of these instead of leaving it a ZIP, so the archive reader would
/// otherwise report it as a corrupt ZIP.
const COMPOUND_FILE_SIGNATURE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

impl ZipPackage<File> {
    pub fn open(path: &Path, max_entry_bytes: u64) -> Result<Self> {
        let mut file = File::open(path)?;
        let mut signature = [0u8; 8];
        if let Ok(()) = file.read_exact(&mut signature)
            && signature == COMPOUND_FILE_SIGNATURE
        {
            return Err(Error::Unsupported(
                "encrypted or legacy binary Office documents are unsupported; access controls are not bypassed".into(),
            ));
        }
        file.rewind()?;
        Self::from_reader(file, max_entry_bytes)
    }
}

impl<R: Read + Seek> ZipPackage<R> {
    /// Open a ZIP package from any bounded, seekable reader.
    ///
    /// This is also the in-memory entry point used by browser/WASM callers;
    /// those callers pass a `Cursor<&[u8]>` and never need a filesystem path.
    pub fn from_reader(reader: R, max_entry_bytes: u64) -> Result<Self> {
        let archive = ZipArchive::new(reader)?;
        validate_zip_entry_count(archive.len())?;
        Ok(Self {
            archive,
            max_entry_bytes,
        })
    }

    pub fn entry_count(&self) -> usize {
        self.archive.len()
    }

    pub fn contains(&mut self, name: &str) -> bool {
        self.archive.by_name(name).is_ok()
    }

    pub fn read(&mut self, name: &str) -> Result<Vec<u8>> {
        self.read_limited(name, self.max_entry_bytes)
    }

    pub fn read_limited(&mut self, name: &str, max_bytes: u64) -> Result<Vec<u8>> {
        let limit = self.max_entry_bytes.min(max_bytes);
        let entry = self.archive.by_name(name)?;
        if entry.size() > limit {
            return Err(Error::LimitExceeded(format!(
                "ZIP entry {name} is {} bytes; maximum is {limit} bytes",
                entry.size(),
            )));
        }
        let capacity = usize::try_from(entry.size()).map_err(|_| {
            Error::LimitExceeded(format!("ZIP entry {name} does not fit in memory"))
        })?;
        let mut bytes = Vec::with_capacity(capacity.min(8 * 1024 * 1024));
        entry
            .take(limit.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > limit {
            return Err(Error::LimitExceeded(format!(
                "ZIP entry {name} expanded beyond {limit} bytes"
            )));
        }
        Ok(bytes)
    }

    pub fn read_optional(&mut self, name: &str) -> Result<Option<Vec<u8>>> {
        self.read_optional_limited(name, self.max_entry_bytes)
    }

    pub fn read_optional_limited(&mut self, name: &str, max_bytes: u64) -> Result<Option<Vec<u8>>> {
        let limit = self.max_entry_bytes.min(max_bytes);
        match self.archive.by_name(name) {
            Ok(entry) => {
                if entry.size() > limit {
                    return Err(Error::LimitExceeded(format!(
                        "ZIP entry {name} is {} bytes; maximum is {} bytes",
                        entry.size(),
                        limit
                    )));
                }
                let mut bytes = Vec::new();
                entry
                    .take(limit.saturating_add(1))
                    .read_to_end(&mut bytes)?;
                if bytes.len() as u64 > limit {
                    return Err(Error::LimitExceeded(format!(
                        "ZIP entry {name} expanded beyond {limit} bytes"
                    )));
                }
                Ok(Some(bytes))
            }
            Err(zip::result::ZipError::FileNotFound) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub fn relationships(&mut self, part_name: &str, max_events: usize) -> Result<Relationships> {
        self.relationships_with_size(part_name, max_events, self.max_entry_bytes)
            .map(|(relationships, _)| relationships)
    }

    pub fn relationships_with_size(
        &mut self,
        part_name: &str,
        max_events: usize,
        max_bytes: u64,
    ) -> Result<(Relationships, usize)> {
        let relationship_path = relationship_part_name(part_name)?;
        let Some(xml) = self.read_optional_limited(&relationship_path, max_bytes)? else {
            return Ok((Relationships::default(), 0));
        };
        let byte_count = xml.len();
        let relationships = Relationships::parse(&xml, part_name, max_events)?;
        Ok((relationships, byte_count))
    }

    pub fn package_relationships(&mut self, max_events: usize) -> Result<(Relationships, usize)> {
        const MAX_PACKAGE_RELATIONSHIPS_BYTES: u64 = 1024 * 1024;
        let xml = self.read_limited("_rels/.rels", MAX_PACKAGE_RELATIONSHIPS_BYTES)?;
        let byte_count = xml.len();
        let relationships = Relationships::parse(&xml, "", max_events)?;
        Ok((relationships, byte_count))
    }
}

impl<'a> ZipPackage<Cursor<&'a [u8]>> {
    pub fn from_bytes(bytes: &'a [u8], max_entry_bytes: u64) -> Result<Self> {
        if bytes.starts_with(&COMPOUND_FILE_SIGNATURE) {
            return Err(Error::Unsupported(
                "encrypted or legacy binary Office documents are unsupported; access controls are not bypassed".into(),
            ));
        }
        Self::from_reader(Cursor::new(bytes), max_entry_bytes)
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Relationships {
    by_id: HashMap<String, Relationship>,
}

#[derive(Clone, Debug)]
struct Relationship {
    target: String,
    relationship_type: String,
    external: bool,
}

impl Relationships {
    fn parse(xml: &[u8], owner_part: &str, max_events: usize) -> Result<Self> {
        let mut reader = Reader::from_reader(xml);
        reader.config_mut().trim_text(true);
        let mut buffer = Vec::new();
        let mut by_id = HashMap::new();
        let mut events = 0usize;
        loop {
            events += 1;
            if events > max_events {
                return Err(Error::LimitExceeded(format!(
                    "relationship XML for {owner_part} exceeds {max_events} events"
                )));
            }
            match reader.read_event_into(&mut buffer)? {
                Event::Empty(start) | Event::Start(start)
                    if local_name(start.name().as_ref()) == b"Relationship" =>
                {
                    let id = attribute(&start, b"Id").unwrap_or_default();
                    let target = attribute(&start, b"Target").unwrap_or_default();
                    let relationship_type = attribute(&start, b"Type").unwrap_or_default();
                    let external = attribute(&start, b"TargetMode")
                        .is_some_and(|mode| mode.eq_ignore_ascii_case("External"));
                    if !id.is_empty() && !target.is_empty() {
                        by_id.insert(
                            id,
                            Relationship {
                                target,
                                relationship_type,
                                external,
                            },
                        );
                    }
                }
                Event::DocType(_) => {
                    return Err(Error::InvalidInput(
                        "relationship XML document type declarations are not supported".into(),
                    ));
                }
                Event::Eof => break,
                _ => {}
            }
            buffer.clear();
        }
        Ok(Self { by_id })
    }

    pub fn target(&self, id: &str, owner_part: &str) -> Option<String> {
        let relationship = self.by_id.get(id)?;
        if relationship.external {
            return None;
        }
        resolve_part_target(owner_part, &relationship.target).ok()
    }

    pub fn external_target(&self, id: &str) -> Option<&str> {
        let relationship = self.by_id.get(id)?;
        relationship
            .external
            .then_some(relationship.target.as_str())
    }

    pub fn ids_of_type(&self, suffix: &str) -> impl Iterator<Item = (&str, &str)> {
        self.by_id.iter().filter_map(move |(id, relationship)| {
            relationship
                .relationship_type
                .ends_with(suffix)
                .then_some((id.as_str(), relationship.target.as_str()))
        })
    }
}

fn relationship_part_name(part_name: &str) -> Result<String> {
    let path = Path::new(part_name);
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| Error::InvalidInput(format!("invalid OOXML part name {part_name}")))?;
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let result = parent.join("_rels").join(format!("{file_name}.rels"));
    Ok(to_zip_path(&result))
}

pub(crate) fn resolve_part_target(owner_part: &str, target: &str) -> Result<String> {
    if target.starts_with('/') {
        return Ok(target.trim_start_matches('/').to_owned());
    }
    let owner = Path::new(owner_part);
    let parent = owner.parent().unwrap_or_else(|| Path::new(""));
    let joined = parent.join(target.replace('\\', "/"));
    let mut normalized = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::Normal(value) => normalized.push(value),
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(Error::InvalidInput(format!(
                        "OOXML relationship escapes package root: {target}"
                    )));
                }
            }
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) => {
                return Err(Error::InvalidInput(format!(
                    "invalid OOXML relationship target: {target}"
                )));
            }
        }
    }
    Ok(to_zip_path(&normalized))
}

fn to_zip_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn validate_zip_entry_count(count: usize) -> Result<()> {
    if count > MAX_ZIP_PACKAGE_ENTRIES {
        return Err(Error::LimitExceeded(format!(
            "ZIP package contains {count} entries; maximum is {MAX_ZIP_PACKAGE_ENTRIES}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{MAX_ZIP_PACKAGE_ENTRIES, Relationships, validate_zip_entry_count};
    use crate::error::Error;

    #[test]
    fn rejects_relationship_xml_document_type_declarations() {
        let xml = br#"<!DOCTYPE Relationships [<!ENTITY target "../../outside.xml">]><Relationships><Relationship Id="r1" Target="&target;" Type="urn:test"/></Relationships>"#;
        assert!(matches!(
            Relationships::parse(xml, "", 100),
            Err(Error::InvalidInput(_))
        ));
    }

    #[test]
    fn rejects_zip_packages_with_excessive_entry_counts() {
        assert!(validate_zip_entry_count(MAX_ZIP_PACKAGE_ENTRIES).is_ok());
        assert!(matches!(
            validate_zip_entry_count(MAX_ZIP_PACKAGE_ENTRIES + 1),
            Err(Error::LimitExceeded(message)) if message.contains("ZIP package contains")
        ));
    }
}
