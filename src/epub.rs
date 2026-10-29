use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read, Seek};
use std::path::Path;

use anyhow::{Context, Result, bail};
use roxmltree::{Document, Node};
use zip::ZipArchive;

use crate::model::Identifier;
use crate::util;

const MAX_XML_BYTES: u64 = 8 * 1024 * 1024;
const MAX_COVER_BYTES: u64 = 25 * 1024 * 1024;
const MAX_CHAPTER_BYTES: u64 = 16 * 1024 * 1024;
const MAX_BOOK_TEXT_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SPINE_ITEMS: usize = 2_000;
const MAX_TOC_ENTRIES: usize = 10_000;
const MAX_TOC_DEPTH: usize = 32;

#[derive(Debug, Clone)]
pub struct Cover {
    pub bytes: Vec<u8>,
    pub extension: String,
}

#[derive(Debug, Clone, Default)]
pub struct EpubMeta {
    pub title: Option<String>,
    pub authors: Vec<String>,
    pub author_sort: Option<String>,
    pub series: Option<String>,
    pub series_index: Option<f32>,
    pub publisher: Option<String>,
    pub published: Option<String>,
    pub language: Option<String>,
    pub description: Option<String>,
    pub subjects: Vec<String>,
    pub identifiers: Vec<Identifier>,
    pub cover: Option<Cover>,
}

pub fn read_metadata(path: &Path) -> Result<EpubMeta> {
    let file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    read_metadata_from(io::BufReader::new(file))
        .with_context(|| format!("cannot read EPUB metadata from {}", path.display()))
}

pub fn read_metadata_from<R: Read + Seek>(reader: R) -> Result<EpubMeta> {
    let mut archive = ZipArchive::new(reader).context("not a valid ZIP/EPUB archive")?;
    let (opf_path, opf_bytes) = read_package(&mut archive)?;
    let opf_text = xml_text(&opf_bytes);
    let opf_doc = Document::parse(&opf_text)
        .with_context(|| format!("package document {opf_path} is not valid XML"))?;

    let mut meta = parse_package(&opf_doc);

    if let Some(cover) = find_cover(&mut archive, &opf_doc, &opf_path) {
        meta.cover = Some(cover);
    }

    Ok(meta)
}

fn read_package<R: Read + Seek>(archive: &mut ZipArchive<R>) -> Result<(String, Vec<u8>)> {
    let container = read_entry(archive, "META-INF/container.xml", MAX_XML_BYTES)
        .context("missing META-INF/container.xml")?;
    let container_text = xml_text(&container);
    let container_doc =
        Document::parse(&container_text).context("META-INF/container.xml is not valid XML")?;

    let opf_path = find_package_path(&container_doc)
        .or_else(|| find_opf_entry(archive))
        .context("the EPUB does not declare an OPF package document")?;

    let opf_bytes = read_entry(archive, &opf_path, MAX_XML_BYTES)
        .with_context(|| format!("missing EPUB package document {opf_path}"))?;
    Ok((opf_path, opf_bytes))
}

fn find_package_path<'a>(doc: &'a Document<'a>) -> Option<String> {
    let rootfiles: Vec<Node<'a, 'a>> = doc
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "rootfile")
        .collect();
    let preferred = rootfiles
        .iter()
        .find(|node| {
            node.attribute("media-type")
                .is_some_and(|kind| kind.contains("oebps-package"))
        })
        .or_else(|| rootfiles.first())?;
    preferred.attribute("full-path").map(str::to_string)
}

fn find_opf_entry<R: Read + Seek>(archive: &mut ZipArchive<R>) -> Option<String> {
    archive
        .file_names()
        .filter_map(|name| name.ok())
        .find(|name| name.to_ascii_lowercase().ends_with(".opf"))
        .map(|name| name.into_owned())
}

fn parse_package(doc: &Document<'_>) -> EpubMeta {
    let mut meta = EpubMeta::default();
    let Some(metadata) = doc
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "metadata")
    else {
        return meta;
    };

    // EPUB 3 puts roles, series info and identifier types in <meta refines="...">.
    let mut refined: HashMap<String, HashMap<String, String>> = HashMap::new();
    for node in metadata
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "meta")
    {
        let Some(target) = node.attribute("refines") else {
            continue;
        };
        let Some(property) = node.attribute("property") else {
            continue;
        };
        let target = target.trim_start_matches('#');
        let value = text_of(node);
        if value.is_empty() {
            continue;
        }
        refined
            .entry(target.to_string())
            .or_default()
            .insert(property.to_string(), value);
    }

    struct Creator {
        name: String,
        role: Option<String>,
        file_as: Option<String>,
    }

    let mut creators: Vec<Creator> = Vec::new();
    for node in metadata.children().filter(|node| node.is_element()) {
        match node.tag_name().name() {
            "title" if meta.title.is_none() => {
                meta.title = non_empty(text_of(node));
            }
            "creator" => {
                let name = text_of(node);
                if name.is_empty() {
                    continue;
                }
                let id = node.attribute("id").map(str::to_string);
                let mut role = attribute_local(node, "role").map(str::to_string);
                let mut file_as = attribute_local(node, "file-as").map(str::to_string);
                if let Some(id) = &id
                    && let Some(values) = refined.get(id)
                {
                    role = role.or_else(|| values.get("role").cloned());
                    file_as = file_as.or_else(|| values.get("file-as").cloned());
                }
                creators.push(Creator {
                    name,
                    role,
                    file_as: file_as.and_then(non_empty),
                });
            }
            "publisher" if meta.publisher.is_none() => {
                meta.publisher = non_empty(text_of(node));
            }
            "date" if meta.published.is_none() => {
                meta.published = non_empty(text_of(node));
            }
            "language" if meta.language.is_none() => {
                meta.language = non_empty(text_of(node));
            }
            "description" if meta.description.is_none() => {
                let raw = text_of(node);
                meta.description = non_empty(util::html_to_text(&raw));
            }
            "subject" => {
                if let Some(subject) = non_empty(text_of(node))
                    && !meta.subjects.iter().any(|existing| existing == &subject)
                {
                    meta.subjects.push(subject);
                }
            }
            "identifier" => {
                let Some(value) = non_empty(text_of(node)) else {
                    continue;
                };
                let id = node.attribute("id").map(str::to_string);
                let mut scheme = attribute_local(node, "scheme").map(str::to_string);
                let mut value = value;
                if let Some(id) = &id
                    && let Some(values) = refined.get(id)
                {
                    scheme = scheme.or_else(|| {
                        values.get("identifier-type").cloned().map(|kind| {
                            if kind.trim() == "15" {
                                "ISBN".to_string()
                            } else {
                                kind
                            }
                        })
                    });
                }
                if scheme.is_none() {
                    if let Some(rest) = value
                        .strip_prefix("urn:isbn:")
                        .or_else(|| value.strip_prefix("ISBN:"))
                        .or_else(|| value.strip_prefix("isbn:"))
                    {
                        scheme = Some("ISBN".to_string());
                        value = rest.trim().to_string();
                    } else if let Some(rest) = value.strip_prefix("urn:uuid:") {
                        scheme = Some("UUID".to_string());
                        value = rest.trim().to_string();
                    }
                }
                if !meta
                    .identifiers
                    .iter()
                    .any(|existing| existing.value == value)
                {
                    meta.identifiers.push(Identifier { scheme, value });
                }
            }
            _ => {}
        }
    }

    // If roles are present, prefer actual authors over editors/translators.
    let has_author_role = creators.iter().any(|creator| {
        creator
            .role
            .as_deref()
            .is_some_and(|role| role.eq_ignore_ascii_case("aut"))
    });
    for creator in creators.iter().filter(|creator| {
        !has_author_role
            || creator
                .role
                .as_deref()
                .is_some_and(|role| role.eq_ignore_ascii_case("aut"))
    }) {
        if !meta
            .authors
            .iter()
            .any(|existing| existing == &creator.name)
        {
            meta.authors.push(creator.name.clone());
        }
    }
    if meta.author_sort.is_none() {
        meta.author_sort = creators
            .iter()
            .find(|creator| meta.authors.contains(&creator.name))
            .and_then(|creator| creator.file_as.clone());
    }

    // EPUB 2 Calibre metadata.
    let mut calibre_series = None;
    let mut calibre_index = None;
    for node in metadata
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "meta")
    {
        match node.attribute("name") {
            Some("calibre:series") => {
                calibre_series = non_empty(node.attribute("content").unwrap_or("").to_string());
            }
            Some("calibre:series_index") => {
                calibre_index = node
                    .attribute("content")
                    .and_then(|value| value.trim().parse::<f32>().ok());
            }
            _ => {}
        }
    }

    // EPUB 3 collections.
    let mut collection = None;
    let mut group_position = None;
    for node in metadata
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "meta")
    {
        match node.attribute("property") {
            Some("belongs-to-collection") if collection.is_none() => {
                collection = non_empty(text_of(node));
            }
            Some("group-position") if group_position.is_none() => {
                group_position = text_of(node).trim().parse::<f32>().ok();
            }
            _ => {}
        }
    }

    meta.series = calibre_series.or(collection);
    meta.series_index = calibre_index.or(group_position);
    meta
}

/// Case-insensitive local-name lookup for namespaced attributes such as
/// `opf:role`, `opf:scheme` and `opf:file-as`.
fn attribute_local<'a>(node: Node<'a, 'a>, name: &str) -> Option<&'a str> {
    node.attributes()
        .find(|attribute| attribute.name().eq_ignore_ascii_case(name))
        .map(|attribute| attribute.value())
}

fn text_of(node: Node<'_, '_>) -> String {
    let mut text = String::new();
    for child in node.descendants().filter(|child| child.is_text()) {
        if let Some(value) = child.text() {
            text.push_str(value);
        }
    }
    if text.is_empty()
        && let Some(value) = node.text()
    {
        text.push_str(value);
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn non_empty(value: String) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn find_cover<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    doc: &Document<'_>,
    opf_path: &str,
) -> Option<Cover> {
    let manifest = doc
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "manifest")?;

    struct Item {
        id: String,
        href: String,
        media_type: String,
        properties: String,
    }

    let items: Vec<Item> = manifest
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "item")
        .filter_map(|node| {
            Some(Item {
                id: node.attribute("id")?.to_string(),
                href: node.attribute("href")?.to_string(),
                media_type: node.attribute("media-type").unwrap_or("").to_string(),
                properties: node.attribute("properties").unwrap_or("").to_string(),
            })
        })
        .collect();

    let metadata = doc
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "metadata");
    let cover_id = metadata
        .into_iter()
        .flat_map(|metadata| metadata.children())
        .filter(|node| node.is_element() && node.tag_name().name() == "meta")
        .find(|node| node.attribute("name") == Some("cover"))
        .and_then(|node| node.attribute("content"))
        .map(str::to_string);

    let chosen = items
        .iter()
        .find(|item| {
            item.properties
                .split_whitespace()
                .any(|p| p == "cover-image")
        })
        .or_else(|| {
            cover_id
                .as_deref()
                .and_then(|id| items.iter().find(|item| item.id == id))
        })
        .or_else(|| {
            items.iter().find(|item| {
                item.media_type.starts_with("image/")
                    && item.href.to_ascii_lowercase().contains("cover")
            })
        })?;

    let extension = cover_extension(&chosen.media_type, &chosen.href)?;
    let bytes = read_cover(archive, &chosen.href, opf_path)?;
    Some(Cover { bytes, extension })
}

fn read_cover<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    href: &str,
    opf_path: &str,
) -> Option<Vec<u8>> {
    let decoded = util::percent_decode(href);
    let mut candidates = Vec::new();
    if let Some(path) = resolve_href(opf_path, &decoded) {
        candidates.push(path);
    }
    if decoded != href
        && let Some(path) = resolve_href(opf_path, href)
    {
        candidates.push(path);
    }
    candidates.dedup();

    for candidate in candidates {
        if let Ok(bytes) = read_optional_entry(archive, &candidate, MAX_COVER_BYTES) {
            return Some(bytes);
        }
    }
    None
}

fn cover_extension(media_type: &str, href: &str) -> Option<String> {
    let extension = match media_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "image/jpeg" | "image/jpg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "image/bmp" => "bmp",
        "image/avif" => "avif",
        _ => href
            .rsplit_once('.')
            .map(|(_, ext)| ext)
            .filter(|ext| {
                !ext.is_empty()
                    && ext.len() <= 5
                    && ext.chars().all(|ch| ch.is_ascii_alphanumeric())
            })
            .unwrap_or("img"),
    };
    Some(extension.to_ascii_lowercase())
}

/// Resolve an href relative to the OPF's directory, rejecting escapes above
/// the archive root.
pub fn resolve_href(opf_path: &str, href: &str) -> Option<String> {
    let href = href.split('#').next()?;
    if href.is_empty() || href.contains('\0') {
        return None;
    }
    let mut parts: Vec<&str> = opf_path.split('/').collect();
    parts.pop(); // drop the OPF file name
    let normalized_href = href.replace('\\', "/");
    for segment in normalized_href.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            segment => parts.push(segment),
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

fn xml_text(bytes: &[u8]) -> String {
    if let Some(bytes) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    if let Some(bytes) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return decode_utf16(bytes, true);
    }
    if let Some(bytes) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return decode_utf16(bytes, false);
    }
    String::from_utf8_lossy(bytes).into_owned()
}

fn decode_utf16(bytes: &[u8], little_endian: bool) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            if little_endian {
                u16::from_le_bytes([pair[0], pair[1]])
            } else {
                u16::from_be_bytes([pair[0], pair[1]])
            }
        })
        .collect();
    char::decode_utf16(units)
        .map(|character| character.unwrap_or('\u{FFFD}'))
        .collect()
}

fn read_entry<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    name: &str,
    max_bytes: u64,
) -> Result<Vec<u8>> {
    let mut entry = archive
        .by_name(name)
        .with_context(|| format!("archive entry {name} not found"))?;
    if entry.size() > max_bytes {
        bail!("archive entry {name} is unexpectedly large");
    }
    let mut buffer = Vec::with_capacity(entry.size().min(max_bytes) as usize);
    entry
        .by_ref()
        .take(max_bytes + 1)
        .read_to_end(&mut buffer)
        .with_context(|| format!("cannot decompress archive entry {name}"))?;
    if buffer.len() as u64 > max_bytes {
        bail!("archive entry {name} is unexpectedly large");
    }
    Ok(buffer)
}

fn read_optional_entry<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    name: &str,
    max_bytes: u64,
) -> Result<Vec<u8>> {
    read_entry(archive, name, max_bytes)
}

#[derive(Debug, Clone, Default)]
pub struct EpubBook {
    pub meta: EpubMeta,
    pub chapters: Vec<Chapter>,
    pub toc: Vec<TocEntry>,
}

#[derive(Debug, Clone)]
pub struct Chapter {
    pub title: Option<String>,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Heading {
        level: u8,
        spans: Vec<InlineSpan>,
    },
    Paragraph(Vec<InlineSpan>),
    Quote(Vec<InlineSpan>),
    ListItem {
        marker: String,
        depth: usize,
        spans: Vec<InlineSpan>,
    },
    Preformatted(String),
    Rule,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineSpan {
    pub text: String,
    pub style: InlineStyle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineStyle {
    Normal,
    Bold,
    Italic,
    BoldItalic,
    Code,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TocEntry {
    pub title: String,
    pub chapter: usize,
    pub depth: usize,
}

/// Read the text content and table of contents of an EPUB. Images, fonts and
/// other non-text resources are ignored.
pub fn read_book(path: &Path) -> Result<EpubBook> {
    let file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    read_book_from(io::BufReader::new(file))
        .with_context(|| format!("cannot read EPUB text from {}", path.display()))
}

pub fn read_book_from<R: Read + Seek>(reader: R) -> Result<EpubBook> {
    let mut archive = ZipArchive::new(reader).context("not a valid ZIP/EPUB archive")?;
    let (opf_path, opf_bytes) = read_package(&mut archive)?;
    let opf_text = xml_text(&opf_bytes);
    let opf_doc = Document::parse(&opf_text)
        .with_context(|| format!("package document {opf_path} is not valid XML"))?;

    let meta = parse_package(&opf_doc);
    let items = manifest_items(&opf_doc);
    let spine = spine_paths(&opf_doc, &opf_path, &items);
    if spine.is_empty() {
        bail!("the EPUB has no reading order (spine)");
    }

    let encrypted = encrypted_content_paths(&mut archive);
    let mut chapters = Vec::new();
    let mut chapter_by_path = HashMap::new();
    let mut total_bytes = 0u64;
    for path in spine {
        if is_encrypted(&path, &encrypted) {
            bail!("this EPUB uses encrypted content (DRM) and cannot be read in clibrary");
        }
        let Ok(bytes) = read_entry(&mut archive, &path, MAX_CHAPTER_BYTES) else {
            continue;
        };
        total_bytes = total_bytes.saturating_add(bytes.len() as u64);
        if total_bytes > MAX_BOOK_TEXT_BYTES {
            bail!("this EPUB's text is unexpectedly large");
        }
        let blocks = extract_xhtml(&bytes);
        if blocks.is_empty() {
            // Covers, title pages that are images only, and similar documents
            // are not useful as reading chapters.
            continue;
        }
        chapter_by_path.insert(path.clone(), chapters.len());
        chapters.push(Chapter {
            title: first_heading(&blocks),
            blocks,
        });
    }
    if chapters.is_empty() {
        bail!("this EPUB does not contain any readable text");
    }

    let raw_toc = read_toc(&mut archive, &opf_doc, &opf_path, &items);
    let mut toc = Vec::new();
    for entry in raw_toc.into_iter().take(MAX_TOC_ENTRIES) {
        let Some(chapter) = find_chapter(&entry.path, &chapter_by_path) else {
            continue;
        };
        if chapters[chapter].title.is_none() {
            chapters[chapter].title = Some(entry.title.clone());
        }
        toc.push(TocEntry {
            title: entry.title,
            chapter,
            depth: entry.depth,
        });
    }

    for (index, chapter) in chapters.iter_mut().enumerate() {
        if chapter.title.is_none() {
            chapter.title = Some(format!("Chapter {}", index + 1));
        }
    }

    if toc.is_empty() {
        toc = chapters
            .iter()
            .enumerate()
            .map(|(index, chapter)| TocEntry {
                title: chapter
                    .title
                    .clone()
                    .unwrap_or_else(|| format!("Chapter {}", index + 1)),
                chapter: index,
                depth: 0,
            })
            .collect();
    }

    Ok(EpubBook {
        meta,
        chapters,
        toc,
    })
}

struct ManifestItem {
    id: String,
    href: String,
    media_type: String,
    properties: String,
}

fn manifest_items(doc: &Document<'_>) -> Vec<ManifestItem> {
    let Some(manifest) = doc
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "manifest")
    else {
        return Vec::new();
    };
    manifest
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "item")
        .filter_map(|node| {
            Some(ManifestItem {
                id: node.attribute("id")?.to_string(),
                href: node.attribute("href")?.to_string(),
                media_type: node.attribute("media-type").unwrap_or("").to_string(),
                properties: node.attribute("properties").unwrap_or("").to_string(),
            })
        })
        .collect()
}

fn spine_paths(doc: &Document<'_>, opf_path: &str, items: &[ManifestItem]) -> Vec<String> {
    let by_id: HashMap<&str, &ManifestItem> =
        items.iter().map(|item| (item.id.as_str(), item)).collect();
    let Some(spine) = doc
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "spine")
    else {
        return Vec::new();
    };

    let mut paths = Vec::new();
    for itemref in spine
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "itemref")
        .take(MAX_SPINE_ITEMS)
    {
        let Some(item) = itemref
            .attribute("idref")
            .and_then(|id| by_id.get(id).copied())
        else {
            continue;
        };
        let is_document = item.media_type.contains("xhtml")
            || item.media_type.contains("html")
            || ["xhtml", "html", "htm"].iter().any(|extension| {
                item.href
                    .to_ascii_lowercase()
                    .ends_with(&format!(".{extension}"))
            });
        if !is_document {
            continue;
        }
        let href = util::percent_decode(&item.href);
        if let Some(path) = resolve_href(opf_path, &href)
            && !paths.contains(&path)
        {
            paths.push(path);
        }
    }
    paths
}

fn encrypted_content_paths<R: Read + Seek>(archive: &mut ZipArchive<R>) -> HashSet<String> {
    let mut paths = HashSet::new();
    let Ok(bytes) = read_entry(archive, "META-INF/encryption.xml", MAX_XML_BYTES) else {
        return paths;
    };
    let text = xml_text(&bytes);
    let Ok(doc) = Document::parse(&text) else {
        return paths;
    };
    for reference in doc
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "CipherReference")
    {
        let Some(uri) = reference.attribute("URI") else {
            continue;
        };
        let decoded = util::percent_decode(uri);
        let lower = decoded.to_ascii_lowercase();
        if [".xhtml", ".html", ".htm"]
            .iter()
            .any(|extension| lower.ends_with(extension))
        {
            paths.insert(decoded.trim_start_matches('/').to_string());
        }
    }
    paths
}

fn is_encrypted(path: &str, encrypted: &HashSet<String>) -> bool {
    if encrypted.is_empty() {
        return false;
    }
    if encrypted.contains(path) {
        return true;
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    encrypted
        .iter()
        .any(|candidate| candidate.rsplit('/').next() == Some(name))
}

struct RawTocEntry {
    title: String,
    path: String,
    depth: usize,
}

fn read_toc<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    opf_doc: &Document<'_>,
    opf_path: &str,
    items: &[ManifestItem],
) -> Vec<RawTocEntry> {
    // EPUB 3 navigation document.
    if let Some(nav) = items.iter().find(|item| {
        item.properties
            .split_whitespace()
            .any(|property| property == "nav")
    }) {
        let nav_href = util::percent_decode(&nav.href);
        if let Some(nav_path) = resolve_href(opf_path, &nav_href)
            && let Ok(bytes) = read_entry(archive, &nav_path, MAX_XML_BYTES)
        {
            let entries = with_xhtml_document(&bytes, |doc| {
                let mut entries = Vec::new();
                parse_nav_document(doc, &nav_path, &mut entries);
                entries
            })
            .unwrap_or_default();
            if !entries.is_empty() {
                return entries;
            }
        }
    }

    // EPUB 2 NCX, located through the spine's `toc` attribute first.
    let toc_id = opf_doc
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "spine")
        .and_then(|spine| spine.attribute("toc"));
    let ncx = toc_id
        .and_then(|id| items.iter().find(|item| item.id == id))
        .or_else(|| {
            items.iter().find(|item| {
                item.media_type.contains("dtbncx")
                    || item.href.to_ascii_lowercase().ends_with(".ncx")
            })
        });
    if let Some(ncx) = ncx {
        let ncx_href = util::percent_decode(&ncx.href);
        if let Some(ncx_path) = resolve_href(opf_path, &ncx_href)
            && let Ok(bytes) = read_entry(archive, &ncx_path, MAX_XML_BYTES)
        {
            return with_xhtml_document(&bytes, |doc| {
                let mut entries = Vec::new();
                if let Some(nav_map) = doc
                    .descendants()
                    .find(|node| node.is_element() && node.tag_name().name() == "navMap")
                {
                    parse_ncx_points(nav_map, &ncx_path, 0, &mut entries);
                }
                entries
            })
            .unwrap_or_default();
        }
    }

    Vec::new()
}

fn parse_nav_document(doc: &Document<'_>, nav_path: &str, entries: &mut Vec<RawTocEntry>) {
    for nav in doc
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "nav")
    {
        let is_toc = attribute_local(nav, "type").is_some_and(|value| {
            value
                .split_whitespace()
                .any(|token| token == "toc" || token == "doc-toc")
        });
        if !is_toc {
            continue;
        }
        let Some(list) = nav
            .descendants()
            .find(|node| node.is_element() && node.tag_name().name() == "ol")
        else {
            continue;
        };
        parse_nav_list(list, nav_path, 0, entries);
        if !entries.is_empty() {
            return;
        }
    }
}

fn parse_nav_list(
    list: Node<'_, '_>,
    nav_path: &str,
    depth: usize,
    entries: &mut Vec<RawTocEntry>,
) {
    if depth > MAX_TOC_DEPTH {
        return;
    }
    for item in list
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "li")
    {
        let label = item
            .children()
            .find(|node| node.is_element() && matches!(node.tag_name().name(), "a" | "span"));
        if let Some(label) = label {
            let title = text_of(label);
            let href = label
                .attribute("href")
                .and_then(|href| resolve_href(nav_path, &util::percent_decode(href)));
            if !title.is_empty()
                && let Some(path) = href
            {
                entries.push(RawTocEntry { title, path, depth });
            }
        }
        for nested in item
            .children()
            .filter(|node| node.is_element() && node.tag_name().name() == "ol")
        {
            parse_nav_list(nested, nav_path, depth + 1, entries);
        }
    }
}

fn parse_ncx_points(
    node: Node<'_, '_>,
    ncx_path: &str,
    depth: usize,
    entries: &mut Vec<RawTocEntry>,
) {
    if depth > MAX_TOC_DEPTH {
        return;
    }
    for point in node
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "navPoint")
    {
        let title = point
            .children()
            .find(|node| node.is_element() && node.tag_name().name() == "navLabel")
            .and_then(|label| {
                label
                    .children()
                    .find(|node| node.is_element() && node.tag_name().name() == "text")
            })
            .map(text_of)
            .unwrap_or_default();
        let path = point
            .children()
            .find(|node| node.is_element() && node.tag_name().name() == "content")
            .and_then(|content| content.attribute("src"))
            .and_then(|src| resolve_href(ncx_path, &util::percent_decode(src)));
        if !title.is_empty()
            && let Some(path) = path
        {
            entries.push(RawTocEntry { title, path, depth });
        }
        parse_ncx_points(point, ncx_path, depth + 1, entries);
    }
}

fn find_chapter(path: &str, chapters: &HashMap<String, usize>) -> Option<usize> {
    if let Some(&index) = chapters.get(path) {
        return Some(index);
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    chapters
        .iter()
        .find(|(candidate, _)| candidate.rsplit('/').next() == Some(name))
        .map(|(_, &index)| index)
}

fn with_xhtml_document<T>(bytes: &[u8], f: impl FnOnce(&Document<'_>) -> T) -> Option<T> {
    let text = prepare_xhtml(bytes);
    let doc = Document::parse(&text).ok()?;
    Some(f(&doc))
}

fn prepare_xhtml(bytes: &[u8]) -> String {
    let text = strip_doctype(&xml_text(bytes));
    normalize_named_entities(&text)
}

fn strip_doctype(input: &str) -> String {
    let lowercase = input.to_ascii_lowercase();
    let Some(start) = lowercase.find("<!doctype") else {
        return input.to_string();
    };
    let mut brackets = 0u32;
    let mut end = None;
    for (offset, ch) in input[start..].char_indices() {
        match ch {
            '[' => brackets += 1,
            ']' => brackets = brackets.saturating_sub(1),
            '>' if brackets == 0 => {
                end = Some(start + offset + ch.len_utf8());
                break;
            }
            _ => {}
        }
    }
    match end {
        Some(end) => format!("{}{}", &input[..start], &input[end..]),
        None => input.to_string(),
    }
}

fn normalize_named_entities(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(position) = rest.find('&') {
        out.push_str(&rest[..position]);
        let after = &rest[position + 1..];
        let Some(end) = after.find(';') else {
            out.push('&');
            rest = after;
            continue;
        };
        let entity = &after[..end];
        let is_xml = matches!(entity, "amp" | "lt" | "gt" | "quot" | "apos");
        if !is_xml && let Some(ch) = util::decode_named_entity(entity) {
            out.push(ch);
            rest = &after[end + 1..];
        } else {
            out.push('&');
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

fn extract_xhtml(bytes: &[u8]) -> Vec<Block> {
    let prepared = prepare_xhtml(bytes);
    if let Ok(doc) = Document::parse(&prepared) {
        let body = doc
            .descendants()
            .find(|node| node.is_element() && node.tag_name().name() == "body")
            .unwrap_or_else(|| doc.root_element());
        let mut blocks = Vec::new();
        walk_blocks(body, &mut blocks, 0, false);
        return blocks;
    }
    fallback_blocks(&xml_text(bytes))
}

fn walk_blocks(node: Node<'_, '_>, blocks: &mut Vec<Block>, depth: usize, quote: bool) {
    for child in node.children() {
        if child.is_text() {
            let mut spans = Vec::new();
            collect_inline(child, InlineStyle::Normal, &mut spans);
            let spans = normalize_spans(spans);
            if !spans.is_empty() {
                blocks.push(if quote {
                    Block::Quote(spans)
                } else {
                    Block::Paragraph(spans)
                });
            }
            continue;
        }
        if !child.is_element() {
            continue;
        }
        let tag = child.tag_name().name().to_ascii_lowercase();
        match tag.as_str() {
            "script" | "style" | "head" | "title" | "nav" | "svg" | "img" | "audio" | "video"
            | "object" | "iframe" => {}
            "p" => push_text_block(child, blocks, quote, None),
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = tag[1..].parse::<u8>().unwrap_or(1);
                let mut spans = Vec::new();
                collect_inline(child, InlineStyle::Bold, &mut spans);
                let spans = normalize_spans(spans);
                if !spans.is_empty() {
                    blocks.push(Block::Heading { level, spans });
                }
            }
            "blockquote" => walk_blocks(child, blocks, depth, true),
            "pre" => {
                let text = raw_text(child);
                let text = text.trim_matches('\n').trim_end().to_string();
                if !text.trim().is_empty() {
                    blocks.push(Block::Preformatted(text));
                }
            }
            "hr" => blocks.push(Block::Rule),
            "ul" | "ol" => {
                let mut number = child
                    .attribute("start")
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or(1);
                for item in child
                    .children()
                    .filter(|node| node.is_element() && node.tag_name().name() == "li")
                {
                    let mut spans = Vec::new();
                    for content in item.children() {
                        if content.is_element() && matches!(content.tag_name().name(), "ul" | "ol")
                        {
                            continue;
                        }
                        collect_inline(content, InlineStyle::Normal, &mut spans);
                    }
                    let spans = normalize_spans(spans);
                    let marker = if tag == "ol" {
                        let marker = format!("{number}.");
                        number += 1;
                        marker
                    } else {
                        "•".to_string()
                    };
                    if !spans.is_empty() {
                        blocks.push(Block::ListItem {
                            marker,
                            depth,
                            spans,
                        });
                    }
                    for nested in item.children().filter(|node| {
                        node.is_element() && matches!(node.tag_name().name(), "ul" | "ol")
                    }) {
                        walk_blocks(nested, blocks, depth + 1, quote);
                    }
                }
            }
            "li" => {
                let mut spans = Vec::new();
                for content in child.children() {
                    if content.is_element() && matches!(content.tag_name().name(), "ul" | "ol") {
                        continue;
                    }
                    collect_inline(content, InlineStyle::Normal, &mut spans);
                }
                let spans = normalize_spans(spans);
                if !spans.is_empty() {
                    blocks.push(Block::ListItem {
                        marker: "•".to_string(),
                        depth,
                        spans,
                    });
                }
                for nested in child.children().filter(|node| {
                    node.is_element() && matches!(node.tag_name().name(), "ul" | "ol")
                }) {
                    walk_blocks(nested, blocks, depth + 1, quote);
                }
            }
            "div" | "section" | "article" | "main" | "header" | "footer" | "aside" | "figure"
            | "figcaption" | "body" | "html" | "table" | "thead" | "tbody" | "tfoot" | "tr"
            | "td" | "th" | "dl" | "dt" | "dd" | "details" | "summary" => {
                walk_blocks(child, blocks, depth, quote);
            }
            _ => {
                // Stray inline content at block level still becomes readable text.
                push_text_block(child, blocks, quote, None);
            }
        }
    }
}

fn push_text_block(
    node: Node<'_, '_>,
    blocks: &mut Vec<Block>,
    quote: bool,
    style: Option<InlineStyle>,
) {
    let mut spans = Vec::new();
    collect_inline(node, style.unwrap_or(InlineStyle::Normal), &mut spans);
    let spans = normalize_spans(spans);
    if spans.is_empty() {
        return;
    }
    blocks.push(if quote {
        Block::Quote(spans)
    } else {
        Block::Paragraph(spans)
    });
}

fn collect_inline(node: Node<'_, '_>, style: InlineStyle, spans: &mut Vec<InlineSpan>) {
    if node.is_text() {
        if let Some(text) = node.text() {
            spans.push(InlineSpan {
                text: text.to_string(),
                style,
            });
        }
        return;
    }
    if !node.is_element() {
        return;
    }
    let tag = node.tag_name().name().to_ascii_lowercase();
    if matches!(
        tag.as_str(),
        "script"
            | "style"
            | "head"
            | "title"
            | "svg"
            | "img"
            | "audio"
            | "video"
            | "object"
            | "iframe"
    ) {
        return;
    }
    if tag == "br" {
        spans.push(InlineSpan {
            text: " ".to_string(),
            style,
        });
        return;
    }
    let style = match tag.as_str() {
        "b" | "strong" => with_bold(style),
        "i" | "em" | "cite" | "dfn" => with_italic(style),
        "code" | "kbd" | "samp" | "tt" => InlineStyle::Code,
        _ => style,
    };
    for child in node.children() {
        collect_inline(child, style, spans);
    }
    if is_block_tag(&tag) {
        spans.push(InlineSpan {
            text: " ".to_string(),
            style,
        });
    }
}

fn is_block_tag(tag: &str) -> bool {
    matches!(
        tag,
        "p" | "div"
            | "section"
            | "article"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "li"
            | "ul"
            | "ol"
            | "blockquote"
            | "pre"
            | "tr"
            | "td"
            | "th"
            | "table"
            | "br"
    )
}

fn with_bold(style: InlineStyle) -> InlineStyle {
    match style {
        InlineStyle::Italic | InlineStyle::BoldItalic => InlineStyle::BoldItalic,
        _ => InlineStyle::Bold,
    }
}

fn with_italic(style: InlineStyle) -> InlineStyle {
    match style {
        InlineStyle::Bold | InlineStyle::BoldItalic => InlineStyle::BoldItalic,
        InlineStyle::Code => InlineStyle::Code,
        _ => InlineStyle::Italic,
    }
}

fn normalize_spans(spans: Vec<InlineSpan>) -> Vec<InlineSpan> {
    let mut out: Vec<InlineSpan> = Vec::new();
    let mut pending_space = false;
    let mut started = false;
    for span in spans {
        let mut text = String::new();
        for ch in span.text.chars() {
            if ch.is_whitespace() {
                if started {
                    pending_space = true;
                }
            } else {
                if pending_space {
                    text.push(' ');
                    pending_space = false;
                }
                started = true;
                text.push(ch);
            }
        }
        if text.is_empty() {
            continue;
        }
        if let Some(last) = out.last_mut()
            && last.style == span.style
        {
            let last: &mut InlineSpan = last;
            last.text.push_str(&text);
            continue;
        }
        out.push(InlineSpan {
            text,
            style: span.style,
        });
    }
    out
}

fn raw_text(node: Node<'_, '_>) -> String {
    let mut text = String::new();
    for child in node.descendants().filter(|child| child.is_text()) {
        if let Some(value) = child.text() {
            text.push_str(value);
        }
    }
    text
}

fn first_heading(blocks: &[Block]) -> Option<String> {
    for block in blocks {
        if let Block::Heading { spans, .. } = block {
            let text = plain_spans(spans);
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    None
}

fn plain_spans(spans: &[InlineSpan]) -> String {
    spans
        .iter()
        .map(|span| span.text.as_str())
        .collect::<Vec<_>>()
        .join("")
        .trim()
        .to_string()
}

fn fallback_blocks(text: &str) -> Vec<Block> {
    let stripped = remove_element_contents(text, "script");
    let stripped = remove_element_contents(&stripped, "style");
    let mut marked = stripped;
    for tag in [
        "p",
        "div",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "li",
        "blockquote",
        "tr",
        "section",
        "article",
    ] {
        marked = replace_ci(&marked, &format!("</{tag}>"), "\n\n");
    }
    for tag in ["br", "br/", "br /"] {
        marked = replace_ci(&marked, &format!("<{tag}>"), "\n");
    }

    let mut blocks = Vec::new();
    for chunk in marked.split('\n') {
        let text = util::html_to_text(chunk);
        if text.is_empty() {
            continue;
        }
        blocks.push(Block::Paragraph(vec![InlineSpan {
            text,
            style: InlineStyle::Normal,
        }]));
    }
    if blocks.is_empty() {
        let text = util::html_to_text(text);
        if !text.is_empty() {
            blocks.push(Block::Paragraph(vec![InlineSpan {
                text,
                style: InlineStyle::Normal,
            }]));
        }
    }
    blocks
}

fn remove_element_contents(input: &str, tag: &str) -> String {
    let lowercase = input.to_ascii_lowercase();
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = String::with_capacity(input.len());
    let mut position = 0;
    while let Some(start) = lowercase[position..].find(&open) {
        let start = position + start;
        let Some(close_start) = lowercase[start..].find(&close) else {
            break;
        };
        out.push_str(&input[position..start]);
        position = start + close_start + close.len();
    }
    out.push_str(&input[position..]);
    out
}

fn replace_ci(haystack: &str, needle: &str, replacement: &str) -> String {
    let lowercase = haystack.to_ascii_lowercase();
    let mut out = String::with_capacity(haystack.len());
    let mut position = 0;
    while let Some(found) = lowercase[position..].find(needle) {
        let found = position + found;
        out.push_str(&haystack[position..found]);
        out.push_str(replacement);
        position = found + needle.len();
    }
    out.push_str(&haystack[position..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{EpubSpec, write_epub, write_epub_with_rootfile};

    #[test]
    fn reads_epub3_metadata_and_cover() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("book.epub");
        let spec = EpubSpec {
            title: Some("Dune".to_string()),
            author: Some("Frank Herbert".to_string()),
            author_sort: Some("Herbert, Frank".to_string()),
            series: Some("Dune".to_string()),
            series_index: Some(1.0),
            subjects: vec!["Science Fiction".to_string(), "Classic".to_string()],
            ..EpubSpec::default()
        };
        write_epub(&path, &spec).unwrap();

        let meta = read_metadata(&path).unwrap();
        assert_eq!(meta.title.as_deref(), Some("Dune"));
        assert_eq!(meta.authors, vec!["Frank Herbert"]);
        assert_eq!(meta.author_sort.as_deref(), Some("Herbert, Frank"));
        assert_eq!(meta.series.as_deref(), Some("Dune"));
        assert_eq!(meta.series_index, Some(1.0));
        assert_eq!(meta.publisher.as_deref(), Some("Test Press"));
        assert_eq!(meta.published.as_deref(), Some("2020-01-02"));
        assert_eq!(meta.language.as_deref(), Some("en"));
        assert_eq!(meta.description.as_deref(), Some("A test book & sample."));
        assert_eq!(meta.subjects, vec!["Science Fiction", "Classic"]);
        assert_eq!(meta.identifiers.len(), 1);
        assert_eq!(meta.identifiers[0].scheme.as_deref(), Some("ISBN"));
        assert_eq!(meta.identifiers[0].value, "9780000000000");
        let cover = meta.cover.expect("cover should be extracted");
        assert_eq!(cover.extension, "jpg");
        assert!(cover.bytes.starts_with(&[0xFF, 0xD8]));
    }

    #[test]
    fn reads_epub2_metadata() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("book.epub");
        let spec = EpubSpec {
            title: Some("Old Book".to_string()),
            author: Some("Ada Lovelace".to_string()),
            author_sort: Some("Lovelace, Ada".to_string()),
            author_role: Some("aut".to_string()),
            series: Some("Classics".to_string()),
            series_index: Some(2.5),
            epub3: false,
            ..EpubSpec::default()
        };
        write_epub(&path, &spec).unwrap();

        let meta = read_metadata(&path).unwrap();
        assert_eq!(meta.title.as_deref(), Some("Old Book"));
        assert_eq!(meta.authors, vec!["Ada Lovelace"]);
        assert_eq!(meta.author_sort.as_deref(), Some("Lovelace, Ada"));
        assert_eq!(meta.series.as_deref(), Some("Classics"));
        assert_eq!(meta.series_index, Some(2.5));
        assert!(meta.cover.is_some());
    }

    #[test]
    fn reports_missing_metadata_without_failing() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("book.epub");
        let spec = EpubSpec {
            title: None,
            author: None,
            publisher: None,
            date: None,
            language: None,
            description: None,
            identifier: None,
            subjects: Vec::new(),
            cover: false,
            ..EpubSpec::default()
        };
        write_epub(&path, &spec).unwrap();
        let meta = read_metadata(&path).unwrap();
        assert!(meta.title.is_none());
        assert!(meta.authors.is_empty());
        assert!(meta.cover.is_none());
    }

    #[test]
    fn reads_epub3_chapters_and_navigation() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("book.epub");
        let spec = EpubSpec {
            chapters: vec![
                crate::fixtures::ChapterSpec::new(
                    "The Beginning",
                    &[
                        "First <em>chapter</em> body.",
                        "A second &nbsp; paragraph with <strong>weight</strong>.",
                    ],
                ),
                crate::fixtures::ChapterSpec::new("The End", &["Last words."]),
            ],
            ..EpubSpec::default()
        };
        write_epub(&path, &spec).unwrap();

        let book = read_book(&path).unwrap();
        assert_eq!(book.meta.title.as_deref(), Some("Test Book"));
        assert_eq!(book.chapters.len(), 2);
        assert_eq!(book.chapters[0].title.as_deref(), Some("The Beginning"));
        assert_eq!(book.toc.len(), 2);
        assert_eq!(book.toc[0].title, "The Beginning");
        assert_eq!(book.toc[0].chapter, 0);
        assert_eq!(book.toc[1].title, "The End");
        assert_eq!(book.toc[1].chapter, 1);

        let first = &book.chapters[0].blocks;
        assert!(matches!(first[0], Block::Heading { level: 1, .. }));
        let Block::Paragraph(spans) = &first[1] else {
            panic!("expected a paragraph");
        };
        assert!(
            spans
                .iter()
                .any(|span| span.style == InlineStyle::Italic && span.text.contains("chapter"))
        );
        let Block::Paragraph(spans) = &first[2] else {
            panic!("expected a paragraph");
        };
        let text = plain_spans(spans);
        assert!(text.contains("second paragraph"), "{text}");
    }

    #[test]
    fn reads_epub2_ncx_navigation() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("book.epub");
        let spec = EpubSpec {
            epub3: false,
            chapters: vec![
                crate::fixtures::ChapterSpec::new("Alpha", &["First."]),
                crate::fixtures::ChapterSpec::new("Beta", &["Second."]),
            ],
            ..EpubSpec::default()
        };
        write_epub(&path, &spec).unwrap();

        let book = read_book(&path).unwrap();
        assert_eq!(book.chapters.len(), 2);
        assert_eq!(
            book.toc
                .iter()
                .map(|entry| entry.title.as_str())
                .collect::<Vec<_>>(),
            vec!["Alpha", "Beta"]
        );
        assert_eq!(book.toc[1].chapter, 1);
    }

    #[test]
    fn extracts_block_structure_and_inline_styles() {
        let html = br#"<?xml version="1.0" encoding="utf-8"?>
<html xmlns="http://www.w3.org/1999/xhtml"><body>
  <h2>Heading</h2>
  <p>Hello <em>world</em>&nbsp;and <strong>friends</strong>.</p>
  <blockquote><p>A quoted line.</p></blockquote>
  <ul><li>First item</li><li>Second <code>item</code></li></ul>
  <pre>  indented
  code</pre>
  <hr/>
</body></html>"#;
        let blocks = extract_xhtml(html);
        assert!(matches!(blocks[0], Block::Heading { level: 2, .. }));
        assert!(matches!(blocks[1], Block::Paragraph(_)));
        assert!(matches!(blocks[2], Block::Quote(_)));
        assert!(matches!(
            &blocks[3],
            Block::ListItem { marker, depth: 0, .. } if marker == "•"
        ));
        assert!(matches!(&blocks[4], Block::ListItem { depth: 0, .. }));
        let Block::ListItem { spans, .. } = &blocks[4] else {
            panic!("expected list item");
        };
        assert!(
            spans
                .iter()
                .any(|span| span.style == InlineStyle::Code && span.text.contains("item"))
        );
        assert!(matches!(&blocks[5], Block::Preformatted(text) if text.contains("indented")));
        assert!(matches!(blocks[6], Block::Rule));
    }

    #[test]
    fn tolerates_doctypes_entities_and_broken_markup() {
        let html = br#"<!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.1//EN"
            "http://www.w3.org/TR/xhtml11/DTD/xhtml11.dtd">
<html xmlns="http://www.w3.org/1999/xhtml"><body>
  <p>Hello&nbsp;there &mdash; friend.</p>
</body></html>"#;
        let blocks = extract_xhtml(html);
        let text = plain_spans(match &blocks[0] {
            Block::Paragraph(spans) => spans,
            other => panic!("expected paragraph, got {other:?}"),
        });
        assert_eq!(text, "Hello there — friend.");

        // Unclosed HTML-style void tags are not valid XML, so this goes through
        // the plain-text fallback instead of failing the whole book.
        let broken = br#"<html><body><p>First line<br>Second line</p></body></html>"#;
        let blocks = extract_xhtml(broken);
        let text = blocks
            .iter()
            .flat_map(|block| match block {
                Block::Paragraph(spans) => spans.iter().map(|span| span.text.as_str()).collect(),
                _ => Vec::new(),
            })
            .collect::<Vec<_>>()
            .join(" ");
        assert!(text.contains("First line"));
        assert!(text.contains("Second line"));
    }

    #[test]
    fn rejects_books_without_readable_text() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("book.epub");
        let spec = EpubSpec {
            chapters: vec![crate::fixtures::ChapterSpec {
                heading: false,
                ..crate::fixtures::ChapterSpec::new("Empty", &[])
            }],
            ..EpubSpec::default()
        };
        write_epub(&path, &spec).unwrap();
        let error = read_book(&path).unwrap_err();
        assert!(format!("{error:#}").contains("readable text"));

        let missing = temporary.path().join("missing.epub");
        assert!(read_book(&missing).is_err());
    }

    #[test]
    fn decodes_utf16_xml() {
        let xml = "<?xml version=\"1.0\" encoding=\"utf-16\"?><a>héllo</a>";
        let mut bytes = vec![0xFF, 0xFE];
        for unit in xml.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        assert!(xml_text(&bytes).contains("<a>héllo</a>"));

        let mut bytes = vec![0xFE, 0xFF];
        for unit in xml.encode_utf16() {
            bytes.extend_from_slice(&unit.to_be_bytes());
        }
        assert!(xml_text(&bytes).contains("<a>héllo</a>"));
    }

    #[test]
    fn rejects_traversing_package_path() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("evil.epub");
        write_epub_with_rootfile(&path, "../outside.opf").unwrap();
        assert!(read_metadata(&path).is_err());
    }

    #[test]
    fn resolves_hrefs_relative_to_opf() {
        assert_eq!(
            resolve_href("OEBPS/content.opf", "images/cover.jpg").as_deref(),
            Some("OEBPS/images/cover.jpg")
        );
        assert_eq!(
            resolve_href("content.opf", "cover.jpg").as_deref(),
            Some("cover.jpg")
        );
        assert_eq!(
            resolve_href("OEBPS/content.opf", "../cover.jpg").as_deref(),
            Some("cover.jpg")
        );
        assert_eq!(resolve_href("content.opf", "../../cover.jpg"), None);
        assert_eq!(
            resolve_href("OEBPS/content.opf", "images/cover%20art.jpg").as_deref(),
            Some("OEBPS/images/cover%20art.jpg")
        );
    }

    #[test]
    fn picks_cover_extension() {
        assert_eq!(
            cover_extension("image/jpeg", "c.jpg").as_deref(),
            Some("jpg")
        );
        assert_eq!(cover_extension("image/png", "x").as_deref(), Some("png"));
        assert_eq!(cover_extension("", "cover.PNG").as_deref(), Some("png"));
        assert_eq!(cover_extension("", "cover").as_deref(), Some("img"));
    }
}
