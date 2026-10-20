//! Resolve leading regional indicators to embedded flag sprite indices.
// Row-major order in assets/flags_atlas.png: 16 columns of 48 x 36 pixels.
static FLAG_CODES: &[&str] = &[
    "ad", "ae", "af", "ag", "ai", "al", "am", "ao", "aq", "ar", "arab", "as", "asean", "at", "au",
    "aw", "ax", "az", "ba", "bb", "bd", "be", "bf", "bg", "bh", "bi", "bj", "bl", "bm", "bn", "bo",
    "bq", "br", "bs", "bt", "bv", "bw", "by", "bz", "ca", "cc", "cd", "cefta", "cf", "cg", "ch",
    "ci", "ck", "cl", "cm", "cn", "co", "cp", "cr", "cu", "cv", "cw", "cx", "cy", "cz", "de", "dg",
    "dj", "dk", "dm", "do", "dz", "eac", "ec", "ee", "eg", "eh", "er", "es", "es-ct", "es-ga",
    "es-pv", "et", "eu", "fi", "fj", "fk", "fm", "fo", "fr", "ga", "gb", "gb-eng", "gb-nir",
    "gb-sct", "gb-wls", "gd", "ge", "gf", "gg", "gh", "gi", "gl", "gm", "gn", "gp", "gq", "gr",
    "gs", "gt", "gu", "gw", "gy", "hk", "hm", "hn", "hr", "ht", "hu", "ic", "id", "ie", "il", "im",
    "in", "io", "iq", "ir", "is", "it", "je", "jm", "jo", "jp", "ke", "kg", "kh", "ki", "km", "kn",
    "kp", "kr", "kw", "ky", "kz", "la", "lb", "lc", "li", "lk", "lr", "ls", "lt", "lu", "lv", "ly",
    "ma", "mc", "md", "me", "mf", "mg", "mh", "mk", "ml", "mm", "mn", "mo", "mp", "mq", "mr", "ms",
    "mt", "mu", "mv", "mw", "mx", "my", "mz", "na", "nc", "ne", "nf", "ng", "ni", "nl", "no", "np",
    "nr", "nu", "nz", "om", "pa", "pc", "pe", "pf", "pg", "ph", "pk", "pl", "pm", "pn", "pr", "ps",
    "pt", "pw", "py", "qa", "re", "ro", "rs", "ru", "rw", "sa", "sb", "sc", "sd", "se", "sg", "sh",
    "sh-ac", "sh-hl", "sh-ta", "si", "sj", "sk", "sl", "sm", "sn", "so", "sr", "ss", "st", "sv",
    "sx", "sy", "sz", "tc", "td", "tf", "tg", "th", "tj", "tk", "tl", "tm", "tn", "to", "tr", "tt",
    "tv", "tw", "tz", "ua", "ug", "um", "un", "us", "uy", "uz", "va", "vc", "ve", "vg", "vi", "vn",
    "vu", "wf", "ws", "xk", "xx", "ye", "yt", "za", "zm", "zw",
];

/// Zero-based sprite index for any asset code (including extended codes), or -1.
pub fn flag_index(code: &str) -> i32 {
    let code = code.to_ascii_lowercase();
    FLAG_CODES
        .binary_search(&code.as_str())
        .map_or(-1, |index| index as i32)
}

pub fn label(name: &str) -> (String, i32) {
    let mut chars = name.trim_start().chars();
    let code = chars
        .next()
        .and_then(regional_letter)
        .zip(chars.next().and_then(regional_letter))
        .map(|(first, second)| format!("{first}{second}"));
    let Some(code) = code else {
        return (name.to_owned(), -1);
    };
    let index = flag_index(&code);
    if index < 0 {
        return (name.to_owned(), -1);
    }
    let text = chars.as_str().trim_start_matches('\u{fe0f}').trim_start();
    (text.to_owned(), index)
}

fn regional_letter(ch: char) -> Option<char> {
    ('\u{1f1e6}'..='\u{1f1ff}')
        .contains(&ch)
        .then(|| (b'a' + (ch as u32 - 0x1f1e6) as u8) as char)
}
