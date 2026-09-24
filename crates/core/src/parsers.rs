//! File → Markdown text + tags. Markdown, PDF, DOCX and HTML, same coverage as the TS parsers.

use std::path::Path;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ParsedDoc {
    pub text: String,
    pub tags: Vec<String>,
}

pub const SUPPORTED_EXTENSIONS: &[&str] = &["md", "pdf", "docx", "html", "htm"];

pub fn extension(path: &Path) -> String {
    path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// Parses file bytes by extension. `None` for unsupported or unparsable input, never a panic.
pub fn parse(path: &Path, bytes: &[u8]) -> Option<ParsedDoc> {
    match extension(path).as_str() {
        "md" => Some(markdown(&String::from_utf8_lossy(bytes))),
        "pdf" => pdf(bytes),
        "docx" => docx(bytes).map(|text| markdown(&text)),
        "html" | "htm" => Some(markdown(&html(&String::from_utf8_lossy(bytes)))),
        _ => None,
    }
}

fn markdown(content: &str) -> ParsedDoc {
    let matter = gray_matter::Matter::<gray_matter::engine::YAML>::new();
    let Ok(parsed) = matter.parse::<serde_json::Value>(content) else {
        return ParsedDoc { text: content.to_string(), tags: vec![] };
    };
    let tags = match parsed.data.as_ref().and_then(|d| d.get("tags")) {
        Some(serde_json::Value::Array(a)) => a.iter().map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string())).collect(),
        Some(serde_json::Value::String(s)) => s.split(|c: char| c == ',' || c.is_whitespace()).filter(|t| !t.is_empty()).map(String::from).collect(),
        _ => vec![],
    };
    let text = if parsed.data.is_some() { parsed.content } else { content.to_string() };
    ParsedDoc { text, tags }
}

fn pdf(bytes: &[u8]) -> Option<ParsedDoc> {
    // pdf-extract panics on some malformed files; a bad PDF must never take the indexer down.
    let text = std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem(bytes)).ok()?.ok()?;
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(ParsedDoc { text, tags: vec![] })
}

/// DOCX body → Markdown: `HeadingN` paragraph styles become `#`×N, runs are concatenated.
fn docx(bytes: &[u8]) -> Option<String> {
    use quick_xml::events::Event;
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).ok()?;
    let mut xml = String::new();
    std::io::Read::read_to_string(&mut zip.by_name("word/document.xml").ok()?, &mut xml).ok()?;
    let mut reader = quick_xml::Reader::from_str(&xml);
    let (mut paras, mut line, mut level, mut in_text) = (Vec::<String>::new(), String::new(), 0usize, false);
    loop {
        match reader.read_event().ok()? {
            Event::Start(e) | Event::Empty(e) => match e.name().as_ref() {
                "w:pStyle" => {
                    let val = e.try_get_attribute("w:val").ok().flatten().map(|a| a.value.to_string()).unwrap_or_default();
                    level = val.strip_prefix("Heading").and_then(|n| n.parse().ok()).filter(|n| (1..=6).contains(n)).unwrap_or(0);
                }
                "w:t" => in_text = true,
                "w:tab" => line.push('\t'),
                "w:br" => line.push('\n'),
                _ => {}
            },
            Event::Text(t) if in_text => line.push_str(&t.xml10_content()),
            Event::GeneralRef(r) if in_text => {
                if let Ok(Some(c)) = r.resolve_char_ref() {
                    line.push(c);
                } else {
                    line.push_str(match &*r { "amp" => "&", "lt" => "<", "gt" => ">", "quot" => "\"", "apos" => "'", _ => "" });
                }
            }
            Event::End(e) => match e.name().as_ref() {
                "w:t" => in_text = false,
                "w:p" => {
                    let text = std::mem::take(&mut line);
                    if !text.trim().is_empty() {
                        paras.push(if level > 0 { format!("{} {}", "#".repeat(level), text.trim()) } else { text });
                    }
                    level = 0;
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    Some(paras.join("\n\n"))
}

/// Readability extracts the main article (strips nav/ads); plain conversion if it finds none.
fn html(content: &str) -> String {
    let article = dom_smoothie::Readability::new(content, None, None).ok().and_then(|mut r| r.parse().ok());
    let body = article.map(|a| a.content.to_string()).unwrap_or_else(|| content.to_string());
    htmd::convert(&body).unwrap_or(body)
}

/// Wikilink targets: `[[T]]`, `[[T|alias]]`, `[[T#section]]`, `[[folder/T]]` → `T`. Deduplicated.
pub fn wikilinks(text: &str) -> Vec<String> {
    static RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\[\[([^\]|#]+)(?:[|#][^\]]+)?\]\]").unwrap());
    let mut out: Vec<String> = vec![];
    for m in RE.captures_iter(text) {
        let t = m[1].trim().rsplit('/').next().unwrap_or("").trim().to_string();
        if !t.is_empty() && !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn md(s: &str) -> ParsedDoc {
        parse(Path::new("/v/n.md"), s.as_bytes()).unwrap()
    }

    #[test]
    fn markdown_without_frontmatter_is_passed_through() {
        let d = md("# T\nbody");
        assert_eq!(d.text, "# T\nbody");
        assert!(d.tags.is_empty());
    }

    #[test]
    fn markdown_frontmatter_is_stripped_and_tags_read_as_list_or_string() {
        let d = md("---\ntags: [ai, research]\ntitle: x\n---\n# Body\ntext");
        assert_eq!(d.tags, vec!["ai", "research"]);
        assert!(d.text.trim_start().starts_with("# Body") && !d.text.contains("title:"));
        assert_eq!(md("---\ntags: ai, research  notes\n---\nx").tags, vec!["ai", "research", "notes"]);
        assert_eq!(md("---\ntags:\n  - one\n  - 2\n---\nx").tags, vec!["one", "2"]);
    }

    #[test]
    fn markdown_with_broken_frontmatter_still_indexes_text() {
        let d = md("---\ntags: [unclosed\n---\nbody text");
        assert!(d.text.contains("body text"));
    }

    #[test]
    fn extension_matching_is_case_insensitive() {
        assert!(parse(Path::new("/v/N.MD"), b"hi").is_some());
        assert!(parse(Path::new("/v/n.txt"), b"hi").is_none());
        assert_eq!(extension(Path::new("/v/a.HTM")), "htm");
    }

    #[test]
    fn invalid_utf8_markdown_is_lossy_not_rejected() {
        let d = parse(Path::new("/v/n.md"), b"caf\xe9 ok").unwrap();
        assert!(d.text.contains("ok"));
    }

    fn docx(body_xml: &str) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            let opts = zip::write::SimpleFileOptions::default();
            z.start_file("word/document.xml", opts).unwrap();
            let xml = format!(r#"<?xml version="1.0"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_xml}</w:body></w:document>"#);
            z.write_all(xml.as_bytes()).unwrap();
            z.finish().unwrap();
        }
        buf.into_inner()
    }

    #[test]
    fn docx_headings_become_markdown_headings() {
        let body = r#"<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Intro</w:t></w:r></w:p>
            <w:p><w:r><w:t xml:space="preserve">Hello </w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t>world</w:t></w:r></w:p>
            <w:p><w:pPr><w:pStyle w:val="Heading2"/></w:pPr><w:r><w:t>Details</w:t></w:r></w:p>
            <w:p><w:r><w:t>More</w:t></w:r><w:r><w:tab/><w:t>tabbed</w:t></w:r></w:p>"#;
        let d = parse(Path::new("/v/a.docx"), &docx(body)).unwrap();
        assert_eq!(d.text, "# Intro\n\nHello world\n\n## Details\n\nMore\ttabbed");
    }

    #[test]
    fn docx_that_is_not_a_zip_returns_none() {
        assert!(parse(Path::new("/v/a.docx"), b"not a zip").is_none());
    }

    #[test]
    fn html_article_is_extracted_as_markdown_with_headings() {
        let html = r#"<html><head><title>Page</title></head><body>
            <nav><a href="/">Home</a> <a href="/x">Menu item</a></nav>
            <article><h1>Forge Notes</h1><p>The anvil rings when the hammer falls on hot iron, and the smith keeps
            working the metal until the blade takes shape. This paragraph is long enough to count as content.</p>
            <h2>Quenching</h2><p>Oil quenching reduces cracking compared to water, which cools the steel too fast
            for thick sections. Readability needs real paragraphs to decide what the article is.</p></article>
            </body></html>"#;
        let d = parse(Path::new("/v/p.html"), html.as_bytes()).unwrap();
        assert!(d.text.contains("## Quenching"), "{}", d.text);
        assert!(d.text.contains("Oil quenching"));
    }

    #[test]
    fn html_fragment_without_article_still_converts() {
        let d = parse(Path::new("/v/p.htm"), b"<p>tiny <b>note</b></p>").unwrap();
        assert!(d.text.contains("tiny") && d.text.contains("note"));
    }

    /// Minimal valid single-page PDF with one text line, offsets computed so the xref is exact.
    fn pdf(text: &str) -> Vec<u8> {
        let stream = format!("BT /F1 12 Tf 72 720 Td ({text}) Tj ET");
        let objs = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_string(),
            format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_string(),
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = vec![];
        for (i, o) in objs.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).bytes());
        for off in offsets {
            out.extend(format!("{off:010} 00000 n \n").bytes());
        }
        out.extend(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).bytes());
        out
    }

    #[test]
    fn pdf_text_is_extracted() {
        let d = parse(Path::new("/v/a.pdf"), &pdf("Hello Anvil")).unwrap();
        assert!(d.text.contains("Hello Anvil"), "{:?}", d.text);
    }

    #[test]
    fn corrupt_pdf_returns_none_instead_of_panicking() {
        assert!(parse(Path::new("/v/a.pdf"), b"%PDF-1.4 garbage").is_none());
    }

    #[test]
    fn wikilinks_cover_aliases_sections_folders_and_dedup() {
        let t = "See [[Alpha]], [[Beta|the beta]], [[Gamma#Part 2]], [[folder/sub/Delta]], [[Alpha]] and [[ ]] [not a link]";
        assert_eq!(wikilinks(t), vec!["Alpha", "Beta", "Gamma", "Delta"]);
        assert!(wikilinks("no links").is_empty());
    }
}
