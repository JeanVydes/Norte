use crate::util::formatters::{Cleaner, Formatter, whitespace::WhitespaceFormatter};
use scraper::{ElementRef, Html, Node, Selector};
use serde_json::json;

pub struct HtmlFormatter;

impl Formatter for HtmlFormatter {
    /// Convert HTML to Markdown with comprehensive tag support
    fn to_markdown(html: &str) -> crate::Result<String> {
        let document = Html::parse_document(html);
        let mut markdown = String::new();
        let mut context = MarkdownContext::default();

        if let Ok(body_selector) = Selector::parse("body") {
            if let Some(body) = document.select(&body_selector).next() {
                Self::process_node_to_markdown(body, &mut markdown, &mut context)?;
            } else {
                Self::process_html_root(&document, &mut markdown, &mut context)?;
            }
        } else {
            Self::process_html_root(&document, &mut markdown, &mut context)?;
        }

        Ok(WhitespaceFormatter::clean_text(&markdown))
    }

    /// Convert Markdown to HTML with common syntax support
    fn from_markdown(markdown: &str) -> crate::Result<String> {
        let mut html = String::from(
            "<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"UTF-8\">\n</head>\n<body>\n",
        );

        let lines: Vec<&str> = markdown.lines().collect();
        let mut i = 0;
        let mut in_code_block = false;
        let mut in_list = false;
        let mut in_blockquote = false;
        let mut paragraph_buffer = String::new();

        // Helper closure to flush paragraph buffer
        let flush_paragraph = |buffer: &mut String, html_out: &mut String| -> crate::Result<()> {
            if !buffer.is_empty() {
                let processed = Self::process_inline_markdown(buffer.trim())?;
                html_out.push_str(&format!("<p>{}</p>\n", processed));
                buffer.clear();
            }
            Ok(())
        };

        while i < lines.len() {
            let line = lines[i];
            let trimmed = line.trim();

            // Code blocks
            if line.starts_with("```") {
                flush_paragraph(&mut paragraph_buffer, &mut html)?;
                if in_code_block {
                    html.push_str("</code></pre>\n");
                    in_code_block = false;
                } else {
                    let lang = line.trim_start_matches("```").trim();
                    if lang.is_empty() {
                        html.push_str("<pre><code>");
                    } else {
                        html.push_str(&format!("<pre><code class=\"language-{}\">", lang));
                    }
                    in_code_block = true;
                }
                i += 1;
                continue;
            }

            if in_code_block {
                // Determine if we need a newline based on previous content
                if html.ends_with("<code>") || html.ends_with("\">") {
                    // Start of block, no newline needed usually
                } else {
                    html.push('\n');
                }
                html.push_str(&Self::escape_html(line));
                i += 1;
                continue;
            }

            // Headers
            if trimmed.starts_with('#') {
                flush_paragraph(&mut paragraph_buffer, &mut html)?;
                let level = trimmed.chars().take_while(|c| *c == '#').count();
                if level <= 6 {
                    let content = trimmed.trim_start_matches('#').trim();
                    let processed = Self::process_inline_markdown(content)?;
                    html.push_str(&format!("<h{}>{}</h{}>\n", level, processed, level));
                    i += 1;
                    continue;
                }
            }

            // Horizontal rule
            if trimmed == "---" || trimmed == "***" || trimmed == "___" {
                flush_paragraph(&mut paragraph_buffer, &mut html)?;
                html.push_str("<hr />\n");
                i += 1;
                continue;
            }

            // Blockquote
            if trimmed.starts_with('>') {
                flush_paragraph(&mut paragraph_buffer, &mut html)?;
                if !in_blockquote {
                    html.push_str("<blockquote>\n");
                    in_blockquote = true;
                }
                let content = trimmed.trim_start_matches('>').trim();
                let processed = Self::process_inline_markdown(content)?;
                html.push_str(&format!("<p>{}</p>\n", processed));
                i += 1;

                if i < lines.len() && !lines[i].trim().starts_with('>') {
                    html.push_str("</blockquote>\n");
                    in_blockquote = false;
                }
                continue;
            } else if in_blockquote {
                html.push_str("</blockquote>\n");
                in_blockquote = false;
            }

            // Unordered lists
            if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ") {
                flush_paragraph(&mut paragraph_buffer, &mut html)?;
                if !in_list {
                    html.push_str("<ul>\n");
                    in_list = true;
                }
                let content = trimmed[2..].trim();
                let processed = Self::process_inline_markdown(content)?;
                html.push_str(&format!("<li>{}</li>\n", processed));
                i += 1;

                // Look ahead for end of list
                if i >= lines.len()
                    || (!lines[i].trim().starts_with("- ")
                        && !lines[i].trim().starts_with("* ")
                        && !lines[i].trim().starts_with("+ "))
                {
                    html.push_str("</ul>\n");
                    in_list = false;
                }
                continue;
            }

            // Ordered lists
            if let Some(pos) = trimmed.find(". ") {
                if trimmed[..pos].chars().all(|c| c.is_ascii_digit()) {
                    flush_paragraph(&mut paragraph_buffer, &mut html)?;
                    if !in_list {
                        html.push_str("<ol>\n");
                        in_list = true;
                    }
                    let content = trimmed[pos + 2..].trim();
                    let processed = Self::process_inline_markdown(content)?;
                    html.push_str(&format!("<li>{}</li>\n", processed));
                    i += 1;

                    // Look ahead for end of list
                    let next_is_ordered = if i < lines.len() {
                        let next_line = lines[i].trim();
                        if let Some(next_pos) = next_line.find(". ") {
                            next_line[..next_pos].chars().all(|c| c.is_ascii_digit())
                        } else {
                            false
                        }
                    } else {
                        false
                    };

                    if !next_is_ordered {
                        html.push_str("</ol>\n");
                        in_list = false;
                    }
                    continue;
                }
            } else if in_list {
                html.push_str("</ol>\n"); // Close generic list if somehow left open
                in_list = false;
            }

            // Empty line - forces paragraph flush
            if trimmed.is_empty() {
                flush_paragraph(&mut paragraph_buffer, &mut html)?;
                i += 1;
                continue;
            }

            // Accumulate text for paragraph
            if !paragraph_buffer.is_empty() {
                paragraph_buffer.push(' ');
            }
            paragraph_buffer.push_str(trimmed);
            i += 1;
        }

        // Final cleanup
        flush_paragraph(&mut paragraph_buffer, &mut html)?;

        if in_code_block {
            html.push_str("</code></pre>\n");
        }
        if in_list {
            html.push_str("</ul>\n"); // Fallback
        }
        if in_blockquote {
            html.push_str("</blockquote>\n");
        }

        html.push_str("</body>\n</html>");
        Ok(html)
    }

    fn to_html(text: &str) -> crate::Result<String> {
        Ok(Self::escape_html(text))
    }

    fn from_html(html: &str) -> crate::Result<String> {
        let document = Html::parse_document(html);
        let mut text = String::new();
        Self::extract_text_recursive(document.root_element(), &mut text)?;
        Ok(WhitespaceFormatter::clean_text(&text))
    }

    fn to_json(text: &str) -> crate::Result<String> {
        // Use serde_json for safe escaping
        let value = json!({
            "content": text
        });
        Ok(value.to_string())
    }

    fn from_json(json: &str) -> crate::Result<String> {
        // Strict parsing logic
        let value: serde_json::Value = serde_json::from_str(json)?;

        value
            .get("content")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| crate::Error::Input("'content' field missing or not a string".into()))
    }
}

#[derive(Default)]
struct MarkdownContext {
    list_depth: usize,
    in_table: bool,
    in_pre: bool,
}

impl HtmlFormatter {
    /// Process HTML node recursively to Markdown
    fn process_node_to_markdown(
        element: ElementRef,
        output: &mut String,
        context: &mut MarkdownContext,
    ) -> crate::Result<()> {
        let tag_name = element.value().name();

        match tag_name {
            // Headers - Process children to preserve formatting (bold/italic within headers)
            "h1" => {
                output.push_str("# ");
                Self::process_children(element, output, context)?;
                output.push_str("\n\n");
            }
            "h2" => {
                output.push_str("## ");
                Self::process_children(element, output, context)?;
                output.push_str("\n\n");
            }
            "h3" => {
                output.push_str("### ");
                Self::process_children(element, output, context)?;
                output.push_str("\n\n");
            }
            "h4" => {
                output.push_str("#### ");
                Self::process_children(element, output, context)?;
                output.push_str("\n\n");
            }
            "h5" => {
                output.push_str("##### ");
                Self::process_children(element, output, context)?;
                output.push_str("\n\n");
            }
            "h6" => {
                output.push_str("###### ");
                Self::process_children(element, output, context)?;
                output.push_str("\n\n");
            }

            // Paragraph
            "p" => {
                Self::process_children(element, output, context)?;
                output.push_str("\n\n");
            }

            // Line break
            "br" => {
                output.push_str("  \n");
            }

            // Horizontal rule
            "hr" => {
                output.push_str("\n---\n\n");
            }

            // Bold
            "strong" | "b" => {
                output.push_str("**");
                Self::process_children(element, output, context)?;
                output.push_str("**");
            }

            // Italic
            "em" | "i" => {
                output.push('*');
                Self::process_children(element, output, context)?;
                output.push('*');
            }

            // Code
            "code" => {
                if !context.in_pre {
                    output.push('`');
                    Self::extract_text_from_element(element, output)?;
                    output.push('`');
                } else {
                    Self::extract_text_from_element(element, output)?;
                }
            }

            // Pre-formatted
            "pre" => {
                context.in_pre = true;
                output.push_str("```\n");
                Self::process_children(element, output, context)?;
                output.push_str("\n```\n\n");
                context.in_pre = false;
            }

            // Blockquote
            "blockquote" => {
                let text = Self::get_element_text(element)?;
                for line in text.lines() {
                    output.push_str("> ");
                    output.push_str(line);
                    output.push('\n');
                }
                output.push('\n');
            }

            // Links
            "a" => {
                if let Some(href) = element.value().attr("href") {
                    output.push('[');
                    Self::process_children(element, output, context)?;
                    output.push_str("](");
                    output.push_str(href);
                    if let Some(title) = element.value().attr("title") {
                        output.push_str(" \"");
                        output.push_str(title);
                        output.push('"');
                    }
                    output.push(')');
                } else {
                    Self::process_children(element, output, context)?;
                }
            }

            // Images
            "img" => {
                output.push_str("![");
                if let Some(alt) = element.value().attr("alt") {
                    output.push_str(alt);
                }
                output.push_str("](");
                if let Some(src) = element.value().attr("src") {
                    output.push_str(src);
                }
                if let Some(title) = element.value().attr("title") {
                    output.push_str(" \"");
                    output.push_str(title);
                    output.push('"');
                }
                output.push(')');
            }

            // Lists
            "ul" => {
                output.push('\n');
                context.list_depth += 1;
                Self::process_children(element, output, context)?;
                context.list_depth -= 1;
                output.push('\n');
            }
            "ol" => {
                output.push('\n');
                context.list_depth += 1;
                Self::process_children(element, output, context)?;
                context.list_depth -= 1;
                output.push('\n');
            }
            "li" => {
                let indent = "  ".repeat(context.list_depth.saturating_sub(1));
                output.push_str(&indent);

                // Check parent to determine list type
                if let Some(parent) = element.parent() {
                    if let Some(parent_elem) = ElementRef::wrap(parent) {
                        if parent_elem.value().name() == "ol" {
                            output.push_str("1. ");
                        } else {
                            output.push_str("- ");
                        }
                    }
                } else {
                    output.push_str("- ");
                }

                Self::process_children(element, output, context)?;
                output.push('\n');
            }

            // Tables
            "table" => {
                context.in_table = true;
                output.push('\n');
                Self::process_children(element, output, context)?;
                output.push('\n');
                context.in_table = false;
            }
            "tr" => {
                output.push('|');
                Self::process_children(element, output, context)?;
                output.push('\n');
            }
            "th" => {
                output.push(' ');
                Self::process_children(element, output, context)?; // Use process children for formatting
                output.push_str(" |");
            }
            "td" => {
                output.push(' ');
                Self::process_children(element, output, context)?; // Use process children for formatting
                output.push_str(" |");
            }

            // Strikethrough
            "s" | "strike" | "del" => {
                output.push_str("~~");
                Self::process_children(element, output, context)?;
                output.push_str("~~");
            }

            // Skip these tags
            "script" | "style" | "noscript" => {}

            // Default: process children
            _ => {
                Self::process_children(element, output, context)?;
            }
        }

        Ok(())
    }

    /// Process children of an element
    fn process_children(
        element: ElementRef,
        output: &mut String,
        context: &mut MarkdownContext,
    ) -> crate::Result<()> {
        for child in element.children() {
            match child.value() {
                Node::Text(text) => {
                    output.push_str(text);
                }
                Node::Element(_) => {
                    if let Some(child_elem) = ElementRef::wrap(child) {
                        Self::process_node_to_markdown(child_elem, output, context)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Extract all text from an element (no recursion into children)
    fn extract_text_from_element(element: ElementRef, output: &mut String) -> crate::Result<()> {
        for text_node in element.text() {
            output.push_str(text_node);
        }
        Ok(())
    }

    /// Get all text from element as a String
    fn get_element_text(element: ElementRef) -> crate::Result<String> {
        Ok(element.text().collect::<String>())
    }

    /// Process HTML root when no body found
    fn process_html_root(
        document: &Html,
        output: &mut String,
        context: &mut MarkdownContext,
    ) -> crate::Result<()> {
        if let Some(root) = document
            .root_element()
            .children()
            .find_map(ElementRef::wrap)
        {
            Self::process_node_to_markdown(root, output, context)?;
        }
        Ok(())
    }

    /// Extract text recursively from HTML
    fn extract_text_recursive(element: ElementRef, output: &mut String) -> crate::Result<()> {
        let tag_name = element.value().name();

        if matches!(tag_name, "script" | "style" | "noscript") {
            return Ok(());
        }

        for child in element.children() {
            match child.value() {
                Node::Text(text) => {
                    output.push_str(text);
                }
                Node::Element(_) => {
                    if let Some(child_elem) = ElementRef::wrap(child) {
                        // Add spacing for block elements
                        if matches!(
                            child_elem.value().name(),
                            "p" | "div" | "br" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "li"
                        ) {
                            output.push(' ');
                        }
                        Self::extract_text_recursive(child_elem, output)?;
                    }
                }
                _ => {}
            }
        }

        Ok(())
    }

    /// Process inline Markdown (bold, italic, links, etc.)
    fn process_inline_markdown(text: &str) -> crate::Result<String> {
        let mut result = String::new();
        let chars: Vec<char> = text.chars().collect();
        let mut i = 0;

        while i < chars.len() {
            // Bold: **text** or __text__
            if i + 1 < chars.len()
                && chars[i] == '*'
                && chars[i + 1] == '*'
                && let Some(end) = Self::find_closing(&chars, i + 2, "**")
            {
                result.push_str("<strong>");
                result.push_str(&chars[i + 2..end].iter().collect::<String>());
                result.push_str("</strong>");
                i = end + 2;
                continue;
            }

            // Italic: *text* or _text_
            if chars[i] == '*' || chars[i] == '_' {
                let marker = chars[i];
                if let Some(end) = Self::find_closing_char(&chars, i + 1, marker) {
                    result.push_str("<em>");
                    result.push_str(&chars[i + 1..end].iter().collect::<String>());
                    result.push_str("</em>");
                    i = end + 1;
                    continue;
                }
            }

            // Code: `text`
            if chars[i] == '`'
                && let Some(end) = Self::find_closing_char(&chars, i + 1, '`')
            {
                result.push_str("<code>");
                result.push_str(&Self::escape_html(
                    &chars[i + 1..end].iter().collect::<String>(),
                ));
                result.push_str("</code>");
                i = end + 1;
                continue;
            }

            // Links: [text](url)
            if chars[i] == '['
                && let Some((text_end, url_start, url_end)) = Self::parse_link(&chars, i)
            {
                let link_text = chars[i + 1..text_end].iter().collect::<String>();
                let url = chars[url_start..url_end].iter().collect::<String>();
                result.push_str(&format!(
                    "<a href=\"{}\">{}</a>",
                    Self::escape_html(&url),
                    Self::escape_html(&link_text)
                ));
                i = url_end + 1;
                continue;
            }

            // Images: ![alt](src)
            if chars[i] == '!'
                && i + 1 < chars.len()
                && chars[i + 1] == '['
                && let Some((text_end, url_start, url_end)) = Self::parse_link(&chars, i + 1)
            {
                let alt = chars[i + 2..text_end].iter().collect::<String>();
                let src = chars[url_start..url_end].iter().collect::<String>();
                result.push_str(&format!(
                    "<img src=\"{}\" alt=\"{}\" />",
                    Self::escape_html(&src),
                    Self::escape_html(&alt)
                ));
                i = url_end + 1;
                continue;
            }

            // Strikethrough: ~~text~~
            if i + 1 < chars.len()
                && chars[i] == '~'
                && chars[i + 1] == '~'
                && let Some(end) = Self::find_closing(&chars, i + 2, "~~")
            {
                result.push_str("<del>");
                result.push_str(&chars[i + 2..end].iter().collect::<String>());
                result.push_str("</del>");
                i = end + 2;
                continue;
            }

            // Escape HTML special characters
            match chars[i] {
                '<' => result.push_str("&lt;"),
                '>' => result.push_str("&gt;"),
                '&' => result.push_str("&amp;"),
                '"' => result.push_str("&quot;"),
                c => result.push(c),
            }

            i += 1;
        }

        Ok(result)
    }

    /// Find closing sequence
    fn find_closing(chars: &[char], start: usize, closing: &str) -> Option<usize> {
        let closing_chars: Vec<char> = closing.chars().collect();
        let len = closing_chars.len();

        for i in start..chars.len() {
            if i + len <= chars.len() && &chars[i..i + len] == closing_chars.as_slice() {
                return Some(i);
            }
        }
        None
    }

    /// Find closing character
    fn find_closing_char(chars: &[char], start: usize, closing: char) -> Option<usize> {
        chars[start..]
            .iter()
            .position(|&c| c == closing)
            .map(|pos| start + pos)
    }

    /// Parse markdown link [text](url)
    fn parse_link(chars: &[char], start: usize) -> Option<(usize, usize, usize)> {
        let text_end = Self::find_closing_char(chars, start + 1, ']')?;
        if text_end + 1 >= chars.len() || chars[text_end + 1] != '(' {
            return None;
        }
        let url_end = Self::find_closing_char(chars, text_end + 2, ')')?;
        Some((text_end, text_end + 2, url_end))
    }

    /// Escape HTML special characters
    fn escape_html(text: &str) -> String {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&#39;")
    }
}
