//! Resolve leading regional indicator pairs without relying on emoji fonts.
use slint::Image;
use std::{cell::RefCell, collections::HashMap};

include!(concat!(env!("OUT_DIR"), "/flag_assets.rs"));

thread_local! {
    static FLAGS: RefCell<HashMap<String, Image>> = RefCell::new(HashMap::new());
}

pub fn label(name: &str) -> (String, Image) {
    let mut chars = name.trim_start().chars();
    let code = chars
        .next()
        .and_then(regional_letter)
        .zip(chars.next().and_then(regional_letter))
        .map(|(first, second)| format!("{first}{second}"));
    let Some(code) = code else {
        return (name.to_owned(), Image::default());
    };
    let Ok(index) = FLAG_ASSETS.binary_search_by_key(&code.as_str(), |(code, _)| *code) else {
        return (name.to_owned(), Image::default());
    };
    let flag = FLAGS.with_borrow_mut(|flags| {
        flags
            .entry(code)
            .or_insert_with(|| Image::load_from_svg_data(FLAG_ASSETS[index].1).unwrap_or_default())
            .clone()
    });
    if flag.size().width == 0 {
        return (name.to_owned(), flag);
    }
    let text = chars.as_str().trim_start_matches('\u{fe0f}').trim_start();
    (text.to_owned(), flag)
}

fn regional_letter(ch: char) -> Option<char> {
    ('\u{1f1e6}'..='\u{1f1ff}')
        .contains(&ch)
        .then(|| (b'a' + (ch as u32 - 0x1f1e6) as u8) as char)
}
