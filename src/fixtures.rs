//! Test-only EPUB fixture builder. Compiled into unit tests and included by
//! the integration tests through a `#[path]` module.
#![allow(dead_code)]

use std::fs::File;
use std::io::Write;
use std::path::Path;

use anyhow::Result;
use zip::CompressionMethod;
use zip::write::SimpleFileOptions;

#[derive(Debug, Clone)]
pub struct ChapterSpec {
    pub title: String,
    pub paragraphs: Vec<String>,
    pub heading: bool,
}

impl ChapterSpec {
    pub fn new(title: &str, paragraphs: &[&str]) -> Self {
        Self {
            title: title.to_string(),
            paragraphs: paragraphs.iter().map(|text| text.to_string()).collect(),
            heading: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EpubSpec {
    pub title: Option<String>,
    pub author: Option<String>,
    pub author_sort: Option<String>,
    pub author_role: Option<String>,
    pub publisher: Option<String>,
    pub date: Option<String>,
    pub language: Option<String>,
    pub description: Option<String>,
    pub identifier: Option<String>,
    pub subjects: Vec<String>,
    pub series: Option<String>,
    pub series_index: Option<f32>,
    pub cover: bool,
    pub epub3: bool,
    pub chapters: Vec<ChapterSpec>,
}

impl Default for EpubSpec {
    fn default() -> Self {
        Self {
            title: Some("Test Book".to_string()),
            author: Some("Test Author".to_string()),
            author_sort: None,
            author_role: None,
            publisher: Some("Test Press".to_string()),
            date: Some("2020-01-02".to_string()),
            language: Some("en".to_string()),
            description: Some("<p>A <b>test</b> book &amp; sample.</p>".to_string()),
            identifier: Some("urn:isbn:9780000000000".to_string()),
            subjects: vec!["Fiction".to_string()],
            series: None,
            series_index: None,
            cover: true,
            epub3: true,
            chapters: Vec::new(),
        }
    }
}

impl EpubSpec {
    pub fn new(title: &str, author: &str) -> Self {
        Self {
            title: Some(title.to_string()),
            author: Some(author.to_string()),
            ..Self::default()
        }
    }
}

pub fn write_epub(path: &Path, spec: &EpubSpec) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let opf_path = "OEBPS/content.opf";
    let cover_item = if spec.cover {
        if spec.epub3 {
            r#"<item id="cover-image" href="images/cover.jpg" media-type="image/jpeg" properties="cover-image"/>"#
        } else {
            r#"<item id="cover-image" href="images/cover.jpg" media-type="image/jpeg"/>"#
        }
    } else {
        ""
    };
    let cover_meta = if spec.cover && !spec.epub3 {
        r#"<meta name="cover" content="cover-image"/>"#
    } else {
        ""
    };

    let mut metadata = String::new();
    if let Some(identifier) = &spec.identifier {
        metadata.push_str(&format!(
            "<dc:identifier id=\"bookid\">{}</dc:identifier>",
            xml_escape(identifier)
        ));
    }
    if let Some(title) = &spec.title {
        metadata.push_str(&format!("<dc:title>{}</dc:title>", xml_escape(title)));
    }
    if let Some(author) = &spec.author {
        let file_as = spec
            .author_sort
            .as_ref()
            .map(|value| {
                if spec.epub3 {
                    format!(
                        "<meta refines=\"#creator\" property=\"file-as\">{}</meta>",
                        xml_escape(value)
                    )
                } else {
                    String::new()
                }
            })
            .unwrap_or_default();
        let attribute_sort = spec
            .author_sort
            .as_ref()
            .filter(|_| !spec.epub3)
            .map(|value| format!(" opf:file-as=\"{}\"", xml_escape(value)))
            .unwrap_or_default();
        let attribute_role = spec
            .author_role
            .as_ref()
            .filter(|_| !spec.epub3)
            .map(|value| format!(" opf:role=\"{}\"", xml_escape(value)))
            .unwrap_or_default();
        let role_meta = spec
            .author_role
            .as_ref()
            .filter(|_| spec.epub3)
            .map(|value| {
                format!(
                    "<meta refines=\"#creator\" property=\"role\">{}</meta>",
                    xml_escape(value)
                )
            })
            .unwrap_or_default();
        metadata.push_str(&format!(
            "<dc:creator id=\"creator\"{attribute_sort}{attribute_role}>{}</dc:creator>{file_as}{role_meta}",
            xml_escape(author)
        ));
    }
    if let Some(publisher) = &spec.publisher {
        metadata.push_str(&format!(
            "<dc:publisher>{}</dc:publisher>",
            xml_escape(publisher)
        ));
    }
    if let Some(date) = &spec.date {
        metadata.push_str(&format!("<dc:date>{}</dc:date>", xml_escape(date)));
    }
    if let Some(language) = &spec.language {
        metadata.push_str(&format!(
            "<dc:language>{}</dc:language>",
            xml_escape(language)
        ));
    }
    if let Some(description) = &spec.description {
        metadata.push_str(&format!(
            "<dc:description>{}</dc:description>",
            xml_escape(description)
        ));
    }
    for subject in &spec.subjects {
        metadata.push_str(&format!("<dc:subject>{}</dc:subject>", xml_escape(subject)));
    }
    if let Some(series) = &spec.series {
        if spec.epub3 {
            metadata.push_str(&format!(
                "<meta property=\"belongs-to-collection\" id=\"series\">{}</meta>\
                 <meta refines=\"#series\" property=\"collection-type\">series</meta>",
                xml_escape(series)
            ));
            if let Some(index) = spec.series_index {
                metadata.push_str(&format!(
                    "<meta refines=\"#series\" property=\"group-position\">{index}</meta>"
                ));
            }
        } else {
            metadata.push_str(&format!(
                "<meta name=\"calibre:series\" content=\"{}\"/>",
                xml_escape(series)
            ));
            if let Some(index) = spec.series_index {
                metadata.push_str(&format!(
                    "<meta name=\"calibre:series_index\" content=\"{index}\"/>"
                ));
            }
        }
    }
    metadata.push_str(cover_meta);

    let chapters = if spec.chapters.is_empty() {
        vec![ChapterSpec::new("Chapter", &["Hello."])]
    } else {
        spec.chapters.clone()
    };

    let version = if spec.epub3 { "3.0" } else { "2.0" };
    let nav = if spec.epub3 {
        r#"<item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>"#
    } else {
        ""
    };
    let ncx = if spec.epub3 {
        ""
    } else {
        r#"<item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>"#
    };
    let spine_toc = if spec.epub3 { "" } else { r#" toc="ncx""# };

    let mut chapter_manifest = String::new();
    let mut spine_items = String::new();
    let mut nav_items = String::new();
    let mut ncx_points = String::new();
    for (index, chapter) in chapters.iter().enumerate() {
        let number = index + 1;
        let href = format!("chapter{number}.xhtml");
        chapter_manifest.push_str(&format!(
            "<item id=\"chapter{number}\" href=\"{href}\" media-type=\"application/xhtml+xml\"/>"
        ));
        spine_items.push_str(&format!("<itemref idref=\"chapter{number}\"/>"));
        nav_items.push_str(&format!(
            "<li><a href=\"{href}\">{}</a></li>",
            xml_escape(&chapter.title)
        ));
        ncx_points.push_str(&format!(
            "<navPoint id=\"navPoint-{number}\" playOrder=\"{number}\">\
             <navLabel><text>{}</text></navLabel><content src=\"{href}\"/></navPoint>",
            xml_escape(&chapter.title)
        ));
    }

    let opf = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="{version}" unique-identifier="bookid">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:opf="http://www.idpf.org/2007/opf">
    {metadata}
  </metadata>
  <manifest>
    {nav}
    {ncx}
    {chapter_manifest}
    {cover_item}
  </manifest>
  <spine{spine_toc}>
    {spine_items}
  </spine>
</package>
"#
    );

    let container = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="{opf_path}" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>
"#
    );

    let mut writer = zip::ZipWriter::new(File::create(path)?);
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let deflated = SimpleFileOptions::default();
    writer.start_file("mimetype", stored)?;
    writer.write_all(b"application/epub+zip")?;
    writer.start_file("META-INF/container.xml", deflated)?;
    writer.write_all(container.as_bytes())?;
    writer.start_file(opf_path, deflated)?;
    writer.write_all(opf.as_bytes())?;
    for (index, chapter) in chapters.iter().enumerate() {
        let mut body = if chapter.heading {
            format!("<h1>{}</h1>", xml_escape(&chapter.title))
        } else {
            String::new()
        };
        for paragraph in &chapter.paragraphs {
            body.push_str(&format!("<p>{paragraph}</p>"));
        }
        let document = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
             <html xmlns=\"http://www.w3.org/1999/xhtml\"><head><title>{}</title></head>\
             <body>{body}</body></html>",
            xml_escape(&chapter.title)
        );
        writer.start_file(format!("OEBPS/chapter{}.xhtml", index + 1), deflated)?;
        writer.write_all(document.as_bytes())?;
    }
    if spec.epub3 {
        writer.start_file("OEBPS/nav.xhtml", deflated)?;
        let document = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
             <html xmlns=\"http://www.w3.org/1999/xhtml\" \
             xmlns:epub=\"http://www.idpf.org/2007/ops\"><body>\
             <nav epub:type=\"toc\"><ol>{nav_items}</ol></nav></body></html>"
        );
        writer.write_all(document.as_bytes())?;
    } else {
        writer.start_file("OEBPS/toc.ncx", deflated)?;
        let document = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
             <ncx xmlns=\"http://www.daisy.org/z3986/2005/ncx/\" version=\"2005-1\">\
             <navMap>{ncx_points}</navMap></ncx>"
        );
        writer.write_all(document.as_bytes())?;
    }
    if spec.cover {
        writer.start_file("OEBPS/images/cover.jpg", deflated)?;
        writer.write_all(&fake_jpeg())?;
    }
    writer.finish()?;
    Ok(())
}

/// Write a structurally valid archive whose container points at an arbitrary
/// (possibly traversing) rootfile path.
pub fn write_epub_with_rootfile(path: &Path, rootfile: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let container = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="{rootfile}" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>
"#
    );
    let opf = r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Traversal</dc:title></metadata>
</package>
"#;
    let mut writer = zip::ZipWriter::new(File::create(path)?);
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let deflated = SimpleFileOptions::default();
    writer.start_file("mimetype", stored)?;
    writer.write_all(b"application/epub+zip")?;
    writer.start_file("META-INF/container.xml", deflated)?;
    writer.write_all(container.as_bytes())?;
    if !rootfile.starts_with("..") {
        writer.start_file(rootfile, deflated)?;
        writer.write_all(opf.as_bytes())?;
    }
    writer.finish()?;
    Ok(())
}

fn fake_jpeg() -> Vec<u8> {
    let mut bytes = vec![
        0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0x00,
    ];
    bytes.extend(std::iter::repeat_n(0x20, 64));
    bytes.extend([0xFF, 0xD9]);
    bytes
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
