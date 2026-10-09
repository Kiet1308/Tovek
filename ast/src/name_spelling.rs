//! The one owner of how a derived name is spelled.
//!
//! Every name the namers build from evidence (a field key, a child or service
//! name, a callee's noun) reads the same way wherever it was found:
//! acronym-aware lowerCamel (`UIListLayout` -> `uiListLayout`, `ID` -> `id`),
//! no private `_` on a binding that is used, builtin globals left spellable,
//! and counters that never run into a name's own digits (`bit32` -> `bit32_2`,
//! never `bit322`). Spelling only: nothing here decides whether a name is free.

/// Builtin globals a derived name must not take, even where the script never
/// reads them. A local `math` or `Vector3` would capture library spellings
/// later passes print, and a local `type`, `error` or `shared` reads as the
/// builtin itself. Names source code commonly gives its own locals (`task`,
/// `time`, `delay`, `wait`) are deliberately absent, and so is `vector`, which
/// is reserved exactly where vector literals print through it. Sorted, for
/// binary search.
const SOFT_RESERVED: &[&str] = &[
    "CFrame", "Color3", "Enum", "Instance", "UDim", "UDim2", "Vector2", "Vector3", "_ENV", "_G",
    "_VERSION", "assert", "bit32", "buffer", "coroutine", "debug", "error", "game", "getfenv",
    "getmetatable", "ipairs", "math", "newproxy", "next", "os", "pairs", "pcall", "plugin", "print",
    "rawequal", "rawget", "rawlen", "rawset", "require", "script", "select", "setfenv",
    "setmetatable", "shared", "string", "table", "tonumber", "tostring", "type", "typeof", "unpack",
    "utf8", "warn", "workspace", "xpcall",
];

/// Whether a derived name would spell a builtin global (see [`SOFT_RESERVED`]).
pub(crate) fn soft_reserved(name: &str) -> bool {
    SOFT_RESERVED.binary_search(&name).is_ok()
}

/// The spelling a derived name takes instead of a soft-reserved builtin: the
/// conventional short synonym (`error` -> `err`, `type` -> `kind`), else the
/// PascalCase token the name was read from (`shared` -> `Shared`, the folder
/// it names). `None` when neither differs from a builtin: the caller suffixes.
pub(crate) fn builtin_alternative(name: &str) -> Option<String> {
    let synonym = match name {
        "error" => Some("err"),
        "type" => Some("kind"),
        "next" => Some("nextNode"),
        "string" => Some("str"),
        "table" => Some("tbl"),
        "select" => Some("selection"),
        _ => None,
    };
    let alternative = match synonym {
        Some(synonym) => synonym.to_string(),
        None => {
            let mut chars = name.chars();
            let first = chars.next()?;
            first.to_ascii_uppercase().to_string() + chars.as_str()
        }
    };
    (!soft_reserved(&alternative)).then_some(alternative)
}

/// `_queuedReject` -> `queuedReject`: the leading `_` marks a private field,
/// which says nothing about a binding that holds its value. Kept when it is
/// the whole word (`_x`, `__index`, `_2d`): there it is part of the spelling.
pub(crate) fn strip_private(name: &str) -> &str {
    match name.strip_prefix('_') {
        Some(rest) if rest.len() >= 2 && rest.starts_with(|c: char| c.is_ascii_alphabetic()) => rest,
        _ => name,
    }
}

/// The `counter`-th spelling of `base`: `part2`, or `bit32_2` when the base
/// already ends in a digit, so the counter never reads as part of the name.
pub(crate) fn suffixed(base: &str, counter: usize) -> String {
    let mut name = String::with_capacity(base.len() + 3);
    push_suffixed(&mut name, base, counter);
    name
}

/// Append [`suffixed`]`(base, counter)` to `out`.
pub(crate) fn push_suffixed(out: &mut String, base: &str, counter: usize) {
    use std::fmt::Write;
    out.push_str(base);
    if base.ends_with(|c: char| c.is_ascii_digit()) {
        out.push('_');
    }
    let _ = write!(out, "{counter}");
}

/// The counter [`suffixed`] appended to `base` to spell `name`, if it did.
pub(crate) fn suffix_of(name: &str, base: &str) -> Option<usize> {
    let rest = name.strip_prefix(base)?;
    let digits = if base.ends_with(|c: char| c.is_ascii_digit()) { rest.strip_prefix('_')? } else { rest };
    if digits.is_empty() || digits.starts_with('0') || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok().filter(|&counter| counter >= 2)
}

/// Lowercase the leading word of an identifier, in place, the way source
/// spells it in lowerCamel:
/// - a leading capital run is an acronym, kept together and lowercased up to
///   the capital that starts the next word (`UIListLayout` -> `uiListLayout`,
///   `HTTPService` -> `httpService`, `IDs` -> `ids`);
/// - a bare acronym is one word (`ID` -> `id`, `GUID` -> `guid`), while a
///   deliberate SCREAMING_CASE constant stays (`DEFAULT_BRUSH`, `HTTP2`);
/// - Roblox types whose two capitals are not an acronym take their
///   conventional spelling (`CFrame` -> `cframe`, `UDim2` -> `udim2`).
///
/// Leading underscores are not part of the word and are left untouched.
pub(crate) fn lower_camel_in_place(chars: &mut [char]) {
    let start = chars.iter().position(|&c| c != '_').unwrap_or(chars.len());
    let word = &mut chars[start..];
    if word.is_empty() {
        return;
    }
    if word.iter().any(char::is_ascii_uppercase) && !word.iter().any(char::is_ascii_lowercase) {
        let letters = word.iter().filter(|c| c.is_ascii_alphabetic()).count();
        let constant = word.contains(&'_') || (word.iter().any(char::is_ascii_digit) && letters >= 3);
        if !constant {
            word.iter_mut().for_each(|c| c.make_ascii_lowercase());
        }
        return;
    }
    for (prefix, spelled) in [("CFrame", "cframe"), ("UDim", "udim")] {
        if word.len() >= prefix.len() && word.iter().zip(prefix.chars()).all(|(&c, p)| c == p) {
            for (slot, c) in word.iter_mut().zip(spelled.chars()) {
                *slot = c;
            }
            return;
        }
    }
    let run = word.iter().take_while(|c| c.is_ascii_uppercase()).count();
    let end = match run {
        0 => 0,
        1 => 1,
        _ => {
            let next_lower = word.get(run).is_some_and(char::is_ascii_lowercase);
            // `IDs`, `NPCsFolder`: a plural `s` belongs to the acronym.
            let plural = word.get(run) == Some(&'s') && !word.get(run + 1).is_some_and(char::is_ascii_lowercase);
            if next_lower && !plural { run - 1 } else { run }
        }
    };
    word[..end].iter_mut().for_each(|c| c.make_ascii_lowercase());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lower(raw: &str) -> String {
        let mut chars: Vec<char> = raw.chars().collect();
        lower_camel_in_place(&mut chars);
        chars.into_iter().collect()
    }

    #[test]
    fn acronyms_are_lowercased_as_one_word() {
        assert_eq!(lower("UIListLayout"), "uiListLayout");
        assert_eq!(lower("UIStroke"), "uiStroke");
        assert_eq!(lower("KFMarkers"), "kfMarkers");
        assert_eq!(lower("HTTPService"), "httpService");
        assert_eq!(lower("IDString"), "idString");
        assert_eq!(lower("NPCRespone"), "npcRespone");
        assert_eq!(lower("IDs"), "ids");
        assert_eq!(lower("NPCsFolder"), "npcsFolder");
        assert_eq!(lower("ID"), "id");
        assert_eq!(lower("GUID"), "guid");
        assert_eq!(lower("UI"), "ui");
        assert_eq!(lower("X"), "x");
        assert_eq!(lower("MAX"), "max");
    }

    #[test]
    fn ordinary_words_and_constants_keep_their_spelling() {
        assert_eq!(lower("TextLabel"), "textLabel");
        assert_eq!(lower("IsA"), "isA");
        assert_eq!(lower("XAxis"), "xAxis");
        assert_eq!(lower("humanoid"), "humanoid");
        assert_eq!(lower("DEFAULT_BRUSH"), "DEFAULT_BRUSH");
        assert_eq!(lower("WAIT_INTERVAL"), "WAIT_INTERVAL");
        assert_eq!(lower("HTTP2"), "HTTP2");
        assert_eq!(lower("Transparency_Duration"), "transparency_Duration");
        assert_eq!(lower("_Private"), "_private");
    }

    #[test]
    fn roblox_types_take_their_conventional_spelling() {
        assert_eq!(lower("CFrame"), "cframe");
        assert_eq!(lower("CFrameValue"), "cframeValue");
        assert_eq!(lower("UDim2"), "udim2");
        assert_eq!(lower("UDim"), "udim");
        assert_eq!(lower("Vector3"), "vector3");
    }

    #[test]
    fn private_underscores_are_stripped_only_from_a_real_stem() {
        assert_eq!(strip_private("_queuedReject"), "queuedReject");
        assert_eq!(strip_private("_x"), "_x");
        assert_eq!(strip_private("__index"), "__index");
        assert_eq!(strip_private("_2d"), "_2d");
        assert_eq!(strip_private("plain"), "plain");
    }

    #[test]
    fn counters_never_run_into_a_names_own_digits() {
        assert_eq!(suffixed("part", 2), "part2");
        assert_eq!(suffixed("bit32", 2), "bit32_2");
        assert_eq!(suffix_of("part2", "part"), Some(2));
        assert_eq!(suffix_of("bit32_3", "bit32"), Some(3));
        assert_eq!(suffix_of("bit323", "bit32"), None);
        assert_eq!(suffix_of("part", "part"), None);
        assert_eq!(suffix_of("part02", "part"), None);
        assert_eq!(suffix_of("part1", "part"), None);
    }

    #[test]
    fn builtins_stay_spellable() {
        assert!(SOFT_RESERVED.windows(2).all(|pair| pair[0] < pair[1]), "SOFT_RESERVED must stay sorted");
        assert!(soft_reserved("math") && soft_reserved("Vector3") && soft_reserved("shared"));
        assert!(!soft_reserved("task") && !soft_reserved("time") && !soft_reserved("vector"));
        assert_eq!(builtin_alternative("error").as_deref(), Some("err"));
        assert_eq!(builtin_alternative("type").as_deref(), Some("kind"));
        assert_eq!(builtin_alternative("shared").as_deref(), Some("Shared"));
        assert_eq!(builtin_alternative("Vector3"), None);
        assert_eq!(builtin_alternative("Enum"), None);
    }
}
