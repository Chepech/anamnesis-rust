//! Heading-aware Markdown chunker, a direct port of the TS `chunker.ts`.
//! Lengths are counted in chars (TS counted UTF-16 units), so accented text never splits mid-char.

#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub text: String,
    pub heading: String,
    pub context_path: String,
    pub chunk_index: usize,
}

fn len(s: &str) -> usize {
    s.chars().count()
}

fn heading(line: &str) -> Option<(usize, &str)> {
    let level = line.bytes().take_while(|b| *b == b'#').count();
    let rest = &line[level..];
    let text = rest.trim();
    ((1..=6).contains(&level) && rest.starts_with(char::is_whitespace) && !text.is_empty()).then_some((level, text))
}

pub fn split_markdown(content: &str, chunk_size: usize, overlap: usize) -> Vec<Chunk> {
    let mut stack: Vec<(usize, String)> = vec![];
    let mut current = String::new();
    let mut blocks: Vec<(String, String, String)> = vec![];
    let mut buffer: Vec<&str> = vec![];
    let crumb = |stack: &[(usize, String)]| stack.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join(" > ");

    for line in content.split('\n') {
        if let Some((level, text)) = heading(line) {
            if !buffer.is_empty() {
                blocks.push((current.clone(), crumb(&stack), buffer.join("\n")));
                buffer.clear();
            }
            while stack.last().is_some_and(|(l, _)| *l >= level) {
                stack.pop();
            }
            stack.push((level, text.to_string()));
            current = text.to_string();
        }
        buffer.push(line);
    }
    if !buffer.is_empty() {
        blocks.push((current, crumb(&stack), buffer.join("\n")));
    }

    let mut chunks = vec![];
    for (heading, context_path, text) in blocks {
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        let parts = if len(text) <= chunk_size { vec![text.to_string()] } else { recursive_split(text, &["\n\n", "\n", ". ", " "], chunk_size, overlap) };
        for sub in parts {
            chunks.push(Chunk { text: sub, heading: heading.clone(), context_path: context_path.clone(), chunk_index: chunks.len() });
        }
    }
    chunks
}

fn recursive_split(text: &str, seps: &[&str], size: usize, overlap: usize) -> Vec<String> {
    if len(text) <= size {
        return vec![text.to_string()];
    }
    let Some((sep, rest)) = seps.split_first() else {
        let chars: Vec<char> = text.chars().collect();
        let step = size.saturating_sub(overlap).max(1);
        return (0..chars.len()).step_by(step).map(|i| chars[i..(i + size).min(chars.len())].iter().collect()).collect();
    };
    let mut out = vec![];
    let mut current = String::new();
    for part in text.split(sep) {
        let candidate = if current.is_empty() { part.to_string() } else { format!("{current}{sep}{part}") };
        if len(&candidate) <= size {
            current = candidate;
        } else {
            if !current.is_empty() {
                out.push(current.trim().to_string());
            }
            if len(part) > size {
                out.extend(recursive_split(part, rest, size, overlap));
                current = String::new();
            } else {
                current = part.to_string();
            }
        }
    }
    if !current.trim().is_empty() {
        out.push(current.trim().to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(c: &[Chunk]) -> Vec<&str> {
        c.iter().map(|c| c.text.as_str()).collect()
    }

    #[test]
    fn content_before_first_heading_has_empty_breadcrumb() {
        let c = split_markdown("intro line\n# Title\nbody", 512, 64);
        assert_eq!(c[0].text, "intro line");
        assert_eq!((c[0].heading.as_str(), c[0].context_path.as_str()), ("", ""));
        assert_eq!(c[1].text, "# Title\nbody", "heading line stays in its block");
        assert_eq!((c[1].heading.as_str(), c[1].context_path.as_str()), ("Title", "Title"));
    }

    #[test]
    fn nested_headings_build_breadcrumbs_and_pop_siblings() {
        let md = "# A\na\n## B\nb\n### C\nc\n## D\nd\n# E\ne";
        let c = split_markdown(md, 512, 64);
        let paths: Vec<&str> = c.iter().map(|c| c.context_path.as_str()).collect();
        assert_eq!(paths, vec!["A", "A > B", "A > B > C", "A > D", "E"]);
        assert_eq!(c[3].heading, "D");
    }

    #[test]
    fn chunk_indexes_are_sequential_and_empty_blocks_skipped() {
        let c = split_markdown("# A\n\n\n# B\ntext\n# C\n   ", 512, 64);
        assert_eq!(texts(&c), vec!["# A", "# B\ntext", "# C"]);
        assert_eq!(c.iter().map(|c| c.chunk_index).collect::<Vec<_>>(), vec![0, 1, 2]);
        assert!(split_markdown("", 512, 64).is_empty());
        assert!(split_markdown(" \n\n ", 512, 64).is_empty());
    }

    #[test]
    fn hashes_without_space_are_not_headings() {
        let c = split_markdown("#tag not heading\n####### seven", 512, 64);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].heading, "");
    }

    #[test]
    fn long_blocks_split_on_paragraphs_first() {
        let para = "word ".repeat(20).trim().to_string(); // 99 chars
        let md = format!("# H\n{para}\n\n{para}\n\n{para}");
        let c = split_markdown(&md, 120, 10);
        assert!(c.len() >= 3);
        assert!(c.iter().all(|c| c.text.chars().count() <= 120));
        assert!(c.iter().all(|c| c.heading == "H"), "sub-chunks keep the block breadcrumb");
    }

    #[test]
    fn unbroken_text_falls_back_to_hard_split_with_overlap() {
        let s = "x".repeat(250);
        let c = split_markdown(&s, 100, 20);
        assert_eq!(c.iter().map(|c| c.text.chars().count()).collect::<Vec<_>>(), vec![100, 100, 90, 10]);
    }

    #[test]
    fn multibyte_text_never_panics_and_respects_char_limit() {
        let s = "ñáé🙂".repeat(100);
        let c = split_markdown(&s, 64, 8);
        assert!(!c.is_empty());
        assert!(c.iter().all(|c| c.text.chars().count() <= 64));
    }

    #[test]
    fn sentence_separator_used_before_words() {
        let sent = "This is one sentence that is fairly long";
        let md = format!("{sent}. {sent}. {sent}.");
        let c = split_markdown(&md, 90, 0);
        assert!(c[0].text.ends_with("long"), "split at '. ' boundary: {:?}", c[0].text);
    }
}
