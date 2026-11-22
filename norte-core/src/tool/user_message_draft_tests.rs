use crate::domain::part::{ExtensionPart, Part};
use crate::tool::{ToolEffect, UserMessageDraft};
use crate::{Error, MediaKind, MediaPart, MediaSource, MimeType};

fn media() -> Part {
    let mime = MimeType::new("image/png").expect("mime");
    Part::Media(MediaPart::new(
        MediaKind::Image,
        MediaSource::Inline(vec![1, 2, 3]),
        mime,
    ))
}

#[test]
fn a_draft_accepts_text_and_media() {
    let draft = UserMessageDraft::new(vec![Part::text("look at this"), media()]).expect("draft");
    assert_eq!(draft.parts().len(), 2);
    assert_eq!(draft.inline_len(), 3);
}

#[test]
fn a_draft_refuses_an_empty_part_list() {
    assert!(matches!(
        UserMessageDraft::new(Vec::new()),
        Err(Error::InvalidInput(_))
    ));
}

#[test]
fn a_draft_refuses_empty_text() {
    assert!(matches!(
        UserMessageDraft::text("   "),
        Err(Error::InvalidInput(_))
    ));
}

#[test]
fn a_draft_refuses_parts_that_are_not_text_or_media() {
    let result = ToolEffect::user_message(vec![Part::Extension(ExtensionPart {
        namespace: "plugin".to_owned(),
        data: serde_json::json!({}),
    })]);
    assert!(matches!(result, Err(Error::InvalidInput(_))));
}
