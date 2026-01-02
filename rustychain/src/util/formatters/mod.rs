pub mod html;
pub mod json;
pub mod markdown;
pub mod whitespace;

pub trait Formatter {
    fn to_markdown(html: &str) -> crate::Result<String>;
    fn from_markdown(markdown: &str) -> crate::Result<String>;

    fn to_json(text: &str) -> crate::Result<String>;
    fn from_json(json: &str) -> crate::Result<String>;

    fn to_html(text: &str) -> crate::Result<String>;
    fn from_html(html: &str) -> crate::Result<String>;
}

pub trait Cleaner {
    fn clean_html(html: &str) -> String;
    fn clean_text(text: &str) -> String;
    fn clean_json(json: &str) -> String;
    fn clean_markdown(markdown: &str) -> String;
}
