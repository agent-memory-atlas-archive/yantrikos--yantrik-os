//! Simple block-level markdown parser for chat messages.
//!
//! Converts LLM markdown output into a flat list of styled blocks
//! that Slint can render with per-block formatting.
//!
//! Handles: code fences, headings, bullet lists, numbered lists, the tool trail (`⚙️ …`, see
//! `trail`). Each block carries its text twice: plain, with the inline markers stripped (what a
//! view that draws one `Text` per block shows — the Lens's bubble), and as the markdown it was
//! written in, which [`styled`] turns into Slint's `StyledText` for a view that draws bold,
//! italic, inline code and links (the Agents pane).

/// A parsed content block with its type and text.
pub struct ParsedBlock {
    pub block_type: &'static str, // "text", "code", "heading", "bullet", "tool"
    /// The block as plain text, inline markers stripped.
    pub text: String,
    /// The same block with its inline markdown kept — `**bold**`, `*italic*`, `` `code` ``, links.
    /// A code block's and a tool block's is its text: nothing in either is markup.
    pub markdown: String,
    /// A `tool` block's call. `None` for every other kind of block.
    pub call: Option<crate::trail::ToolCall>,
}

/// The block being gathered, line by line: its plain text and its markdown side by side.
#[derive(Default)]
struct Gathering {
    text: String,
    markdown: String,
}

impl Gathering {
    fn push(&mut self, separator: char, text: &str, markdown: &str) {
        if !self.text.is_empty() {
            self.text.push(separator);
        }
        self.text.push_str(text);
        if !self.markdown.is_empty() {
            self.markdown.push(separator);
        }
        self.markdown.push_str(markdown);
    }

    fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

/// Parse markdown content into renderable blocks.
pub fn parse_blocks(content: &str) -> Vec<ParsedBlock> {
    let mut blocks = Vec::new();
    let mut current_type = "text";
    let mut current = Gathering::default();
    let mut in_code_block = false;

    let mut lines = content.lines().peekable();
    while let Some(line) = lines.next() {
        // Code fence toggle
        if line.trim_start().starts_with("```") {
            if in_code_block {
                // End code block
                flush(&mut blocks, &mut current, "code");
                in_code_block = false;
            } else {
                // Flush current text, start code block
                flush(&mut blocks, &mut current, current_type);
                current_type = "text";
                in_code_block = true;
            }
            continue;
        }

        if in_code_block {
            current.push('\n', line, line);
            continue;
        }

        // A tool call, as the trail writes it: its own block, never folded into the paragraph
        // around it. The panel used to show `⚙️ mcp_yantrik_os_os_act...` as a line of prose and
        // the arguments, when a harness sent them, as raw JSON (#125).
        if crate::trail::is_trail(line) {
            if let Some((call, took_next)) = crate::trail::parse(line, lines.peek().copied()) {
                if took_next {
                    lines.next();
                }
                flush(&mut blocks, &mut current, current_type);
                current_type = "text";
                let summary = call.summary();
                blocks.push(ParsedBlock { block_type: "tool", markdown: summary.clone(), text: summary, call: Some(call) });
                continue;
            }
        }

        let trimmed = line.trim();

        // Empty line → paragraph break
        if trimmed.is_empty() {
            flush(&mut blocks, &mut current, current_type);
            current_type = "text";
            continue;
        }

        // Heading (### > ## > #)
        if let Some(heading_text) = strip_heading(trimmed) {
            flush(&mut blocks, &mut current, current_type);
            current_type = "text";
            blocks.push(ParsedBlock {
                block_type: "heading",
                text: strip_inline(&heading_text),
                markdown: heading_text,
                call: None,
            });
            continue;
        }

        // Bullet list item (- item, * item, • item)
        if let Some(bullet_text) = strip_bullet(trimmed) {
            if current_type != "bullet" {
                flush(&mut blocks, &mut current, current_type);
            }
            current_type = "bullet";
            current.push(
                '\n',
                &format!("\u{2022} {}", strip_inline(&bullet_text)),
                &format!("\u{2022} {bullet_text}"),
            );
            continue;
        }

        // Numbered list (1. item, 2. item, etc.)
        if let Some((num, list_text)) = strip_numbered(trimmed) {
            if current_type != "bullet" {
                flush(&mut blocks, &mut current, current_type);
            }
            current_type = "bullet";
            current.push('\n', &format!("{}. {}", num, strip_inline(&list_text)), &format!("{num}. {list_text}"));
            continue;
        }

        // Normal text paragraph
        if current_type != "text" {
            flush(&mut blocks, &mut current, current_type);
        }
        current_type = "text";
        current.push(' ', &strip_inline(trimmed), trimmed);
    }

    // Flush remaining content
    flush(&mut blocks, &mut current, if in_code_block { "code" } else { current_type });

    // If no blocks were parsed, return content as single text block
    if blocks.is_empty() && !content.trim().is_empty() {
        blocks.push(ParsedBlock {
            block_type: "text",
            text: strip_inline(content.trim()),
            markdown: content.trim().to_string(),
            call: None,
        });
    }

    blocks
}

fn flush(blocks: &mut Vec<ParsedBlock>, current: &mut Gathering, block_type: &'static str) {
    if !current.is_empty() {
        let Gathering { text, markdown } = std::mem::take(current);
        blocks.push(ParsedBlock {
            block_type,
            text: text.trim_end().to_string(),
            markdown: markdown.trim_end().to_string(),
            call: None,
        });
    }
}

/// A block's inline markdown as Slint's `StyledText`: bold, italic, inline code, strikethrough and
/// links drawn inside one wrapped text element, which a plain `Text` cannot do.
///
/// A paragraph's and a list's markdown is read; any other block is its plain text, since a
/// heading is drawn in a heading's own weight and nothing in a code block is markup. What
/// `StyledText` does not take — a raw HTML tag, a block quote, a rule — falls back to the block's
/// plain text with the markers stripped, the Lens's reading, so no block is lost to a parse error.
///
/// An unclosed marker, which is what a paragraph looks like mid-stream (`it is **very`), stays
/// the characters it is — CommonMark leaves an unmatched `**` as text — and the chunk that closes
/// it turns the run bold. Nothing is held back, and the block is as tall as its text.
pub fn styled(block: &ParsedBlock) -> slint::StyledText {
    match block.block_type {
        "text" | "bullet" => slint::StyledText::from_markdown(&block.markdown)
            .unwrap_or_else(|_| slint::StyledText::from_plain_text(&block.text)),
        _ => slint::StyledText::from_plain_text(&block.text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A call in the trail is a block of its own kind, so the bubble can draw it as the call —
    /// name, what it touched, arguments — rather than as a sentence that happens to start with
    /// a gear. On the version this fixes, it came out as a "text" block reading
    /// `⚙️ os_act studio.generate {"args":{…}}`.
    #[test]
    fn a_tool_call_in_an_answer_is_its_own_block_with_its_arguments_on_it() {
        let blocks = parse_blocks(
            "Making it now.\n⚙️ os_act studio.generate {\"args\":{\"prompt\":\"a red kite\"}}\n\nDone: one picture.",
        );
        let kinds: Vec<&str> = blocks.iter().map(|b| b.block_type).collect();
        assert_eq!(kinds, vec!["text", "tool", "text"], "{kinds:?}");
        assert_eq!(blocks[1].text, "os_act studio.generate prompt=\"a red kite\"");
        assert_eq!(blocks[2].text, "Done: one picture.");
    }

    #[test]
    fn a_tool_calls_whole_arguments_ride_with_the_block_and_prose_has_none() {
        let blocks = parse_blocks("⚙️ os_act notes.new_note {\"args\":{\"body\":\"the whole body\"}}\n\nSaved.");
        let call = blocks[0].call.as_ref().expect("a tool block carries its call");
        assert!(call.detail().contains("\"body\": \"the whole body\""));
        assert!(blocks[1].call.is_none());
    }

    /// Hermes' verbose form puts the arguments on the line after the call. They belong to the
    /// call, not to the paragraph.
    #[test]
    fn arguments_on_the_next_line_are_not_shown_as_a_paragraph_of_json() {
        let blocks = parse_blocks(
            "⚙️ mcp_yantrik_os_os_act(['app', 'action', 'args'])\n{\"app\": \"studio\", \"action\": \"generate\", \"args\": {\"prompt\": \"a red kite\"}}\nDone.",
        );
        let kinds: Vec<&str> = blocks.iter().map(|b| b.block_type).collect();
        assert_eq!(kinds, vec!["tool", "text"], "{kinds:?}");
        assert_eq!(blocks[0].text, "mcp_yantrik_os_os_act studio.generate prompt=\"a red kite\"");
    }

    #[test]
    fn a_gear_inside_a_code_fence_stays_code() {
        let blocks = parse_blocks("```\n⚙️ os_apps\n```");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].block_type, "code");
    }

    /// The shape every catalog role's answer takes (#190): headings, bold labels, italics,
    /// backticks, a list, a fence. Each block keeps the markdown it was written in beside the
    /// plain text, and a code block's text is its markdown, markers and all.
    #[test]
    fn each_block_keeps_its_markdown_beside_its_plain_text() {
        let blocks = parse_blocks(
            "## Strongest **point**\n\nThe **How:** is *slow* and `cargo test`\nruns it.\n\n\
             - one **bold** item\n- two\n1. first\n\n```\nlet x = **y**;\n```",
        );
        let kinds: Vec<&str> = blocks.iter().map(|b| b.block_type).collect();
        assert_eq!(kinds, vec!["heading", "text", "bullet", "code"], "{kinds:?}");
        assert_eq!((blocks[0].text.as_str(), blocks[0].markdown.as_str()), ("Strongest point", "Strongest **point**"));
        assert_eq!(blocks[1].text, "The How: is slow and cargo test runs it.");
        assert_eq!(blocks[1].markdown, "The **How:** is *slow* and `cargo test` runs it.");
        assert_eq!(blocks[2].text, "\u{2022} one bold item\n\u{2022} two\n1. first");
        assert_eq!(blocks[2].markdown, "\u{2022} one **bold** item\n\u{2022} two\n1. first");
        assert_eq!((blocks[3].text.as_str(), blocks[3].markdown.as_str()), ("let x = **y**;", "let x = **y**;"));
    }

    /// What `StyledText` holds, as far as a test can see it: its Debug form names each span's style.
    fn styles(text: &slint::StyledText) -> String {
        format!("{text:?}")
    }

    #[test]
    fn a_paragraph_and_a_list_are_drawn_with_their_bold_italic_and_code() {
        let blocks = parse_blocks("The **How:** is *slow* and `cargo` runs.\n\n- one **bold**\n- two");
        let prose = styled(&blocks[0]);
        for style in ["Strong", "Emphasis", "Code"] {
            assert!(styles(&prose).contains(style), "{style} missing: {prose:?}");
        }
        assert_ne!(prose, slint::StyledText::from_plain_text(&blocks[0].text), "it is not the plain text");
        let list = styled(&blocks[1]);
        assert!(styles(&list).contains("Strong"), "{list:?}");
        assert!(styles(&list).contains("\u{2022} two"), "each item is its own line: {list:?}");
    }

    /// Mid-stream a paragraph can end inside a marker. The unclosed `**` is text — the layout is the
    /// text's, nothing is held back — and the chunk that closes it makes the run bold.
    #[test]
    fn an_unclosed_marker_mid_stream_is_text_until_it_closes() {
        let partial = parse_blocks("It is **very");
        assert_eq!(partial.len(), 1);
        let drawn = styled(&partial[0]);
        assert_eq!(drawn, slint::StyledText::from_plain_text("It is **very"), "{drawn:?}");
        let also = styled(&parse_blocks("Run `cargo")[0]);
        assert_eq!(also, slint::StyledText::from_plain_text("Run `cargo"), "{also:?}");
        let closed = styled(&parse_blocks("It is **very** slow")[0]);
        assert!(styles(&closed).contains("Strong"), "{closed:?}");
    }

    /// What `StyledText` refuses — raw HTML, a block quote — is still drawn: as the plain text the
    /// Lens shows. And a heading or a code block is never read as markup.
    #[test]
    fn what_styled_text_cannot_take_falls_back_to_the_plain_text() {
        for source in ["a <div>b</div> **c**", "> quoted **here**"] {
            let block = &parse_blocks(source)[0];
            assert_eq!(styled(block), slint::StyledText::from_plain_text(&block.text), "{source}");
        }
        let code = &parse_blocks("```\n**not bold**\n```")[0];
        assert_eq!(styled(code), slint::StyledText::from_plain_text("**not bold**"));
        let heading = &parse_blocks("# A `title`")[0];
        assert_eq!(styled(heading), slint::StyledText::from_plain_text("A title"));
    }
}

fn strip_heading(line: &str) -> Option<String> {
    if let Some(rest) = line.strip_prefix("### ") {
        Some(rest.trim().to_string())
    } else if let Some(rest) = line.strip_prefix("## ") {
        Some(rest.trim().to_string())
    } else if let Some(rest) = line.strip_prefix("# ") {
        Some(rest.trim().to_string())
    } else {
        None
    }
}

fn strip_bullet(line: &str) -> Option<String> {
    if let Some(rest) = line.strip_prefix("- ") {
        Some(rest.to_string())
    } else if let Some(rest) = line.strip_prefix("* ") {
        Some(rest.to_string())
    } else {
        // Also match Unicode bullet (•)
        let bullet = "\u{2022} ";
        line.strip_prefix(bullet).map(|rest| rest.to_string())
    }
}

fn strip_numbered(line: &str) -> Option<(String, String)> {
    let num_end = line.find(". ")?;
    let num_part = &line[..num_end];
    if num_part.len() <= 3 && num_part.chars().all(|c| c.is_ascii_digit()) {
        Some((num_part.to_string(), line[num_end + 2..].to_string()))
    } else {
        None
    }
}

/// Strip inline markdown markers: **bold**, *italic*, `code`.
fn strip_inline(text: &str) -> String {
    let mut result = text.to_string();

    // Bold: **text**
    while let Some(start) = result.find("**") {
        if let Some(end) = result[start + 2..].find("**") {
            let inner = result[start + 2..start + 2 + end].to_string();
            result = format!("{}{}{}", &result[..start], inner, &result[start + 2 + end + 2..]);
        } else {
            break;
        }
    }

    // Italic: *text* (single asterisks remaining after bold removal)
    while let Some(start) = result.find('*') {
        if let Some(end) = result[start + 1..].find('*') {
            if end > 0 {
                let inner = result[start + 1..start + 1 + end].to_string();
                result =
                    format!("{}{}{}", &result[..start], inner, &result[start + 1 + end + 1..]);
            } else {
                break;
            }
        } else {
            break;
        }
    }

    // Inline code: `text`
    while let Some(start) = result.find('`') {
        if let Some(end) = result[start + 1..].find('`') {
            let inner = result[start + 1..start + 1 + end].to_string();
            result = format!("{}{}{}", &result[..start], inner, &result[start + 1 + end + 1..]);
        } else {
            break;
        }
    }

    result
}
