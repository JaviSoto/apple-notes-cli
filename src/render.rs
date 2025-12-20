use crate::model::Note;
use html2md::parse_html;
use termimad::MadSkin;

pub fn note_to_markdown(note: &Note) -> String {
    let body_md = html_to_markdown(&note.body_html);
    format!("# {}\n\n{}", note.title, body_md.trim())
}

pub fn html_to_markdown(html: &str) -> String {
    parse_html(html)
}

pub fn render_markdown(markdown: &str) -> String {
    let skin = MadSkin::default();
    skin.term_text(markdown).to_string()
}

pub fn text_to_html(text: &str) -> String {
    // Notes stores body as HTML. Wrap plain text in <div> blocks and escape.
    let mut out = String::new();
    for line in text.lines() {
        out.push_str("<div>");
        out.push_str(&escape_html(line));
        out.push_str("</div>\n");
    }
    if out.is_empty() {
        "<div></div>\n".to_string()
    } else {
        out
    }
}

pub fn markdown_to_html(markdown: &str) -> String {
    // Keep it simple and reliable: render markdown to HTML and wrap in a container.
    let html = comrak::markdown_to_html(markdown, &comrak::Options::default());
    format!("<div>{}</div>", html)
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_to_html_wraps_lines_and_escapes() {
        let html = text_to_html("a<b\nc&d");
        assert!(html.contains("&lt;b"));
        assert!(html.contains("&amp;"));
        assert!(html.contains("<div>"));
    }

    #[test]
    fn html_to_markdown_basic() {
        let md = html_to_markdown("<div>Hello</div>");
        assert!(md.to_lowercase().contains("hello"));
    }
}
