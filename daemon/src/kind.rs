//! Classification of clipboard content.
//!
//! The kind is a presentation hint the UI uses to pick a shape to draw. It is
//! never a second source of truth: the set of mime types an entry holds is
//! what decides what can actually be served back.

/// How the UI should present an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text,
    Link,
    Color,
    Image,
    Files,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Text => "text",
            Kind::Link => "link",
            Kind::Color => "color",
            Kind::Image => "image",
            Kind::Files => "files",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "text" => Kind::Text,
            "link" => Kind::Link,
            "color" => Kind::Color,
            "image" => Kind::Image,
            "files" => Kind::Files,
            _ => return None,
        })
    }
}

/// Mime types that mean "this is a secret, do not remember it".
///
/// Password managers, browsers in private mode and remote desktop clients all
/// set one of these alongside the content. An entry offering any of them is
/// dropped whole — not stored and then hidden, because an archive that holds a
/// password it promises not to show is still an archive holding a password.
///
/// The names are the ones the ecosystem settled on rather than a standard:
/// KDE's hint is what KeePassXC, KWallet and Klipper agree on, and the
/// NSPasteboard type is its macOS equivalent.
const SENSITIVE: &[&str] = &[
    "x-kde-passwordmanagerhint",
    "application/x-nspasteboard-concealed-type",
    "org.nspasteboard.concealedtype",
    "text/x-moz-password",
    "x-keepassxc-password",
    "password",
    "secret",
];

pub fn is_sensitive(mime: &str) -> bool {
    let lowered = mime.to_ascii_lowercase();
    SENSITIVE.iter().any(|hint| lowered == *hint)
}

/// True when any representation marks the whole entry as a secret.
pub fn any_sensitive<'a>(mimes: impl IntoIterator<Item = &'a str>) -> bool {
    mimes.into_iter().any(is_sensitive)
}

/// Serving preference, lowest first.
///
/// This decides only which representation a `fetch` without a mime returns.
/// Restoring an entry to the clipboard offers every representation it holds,
/// so the pasting application still negotiates for itself.
pub fn rank(mime: &str) -> i32 {
    match lower(mime).as_str() {
        // A picture is the most specific thing an entry can be, and the one a
        // text fallback would silently degrade.
        "image/png" => 0,
        "image/webp" => 1,
        "image/jpeg" | "image/jpg" => 2,
        "image/gif" => 3,
        "image/bmp" => 4,
        // File references outrank the text that spells them out.
        "x-special/gnome-copied-files" => 10,
        "x-special/nautilus-clipboard" => 11,
        "text/uri-list" => 12,
        // Rich text before the flattening of it.
        "text/html" => 20,
        "text/rtf" | "application/rtf" => 21,
        "text/plain;charset=utf-8" => 30,
        "utf8_string" => 31,
        "text/plain" => 32,
        "string" | "text" => 33,
        other if other.starts_with("image/") => 5,
        other if other.starts_with("text/") => 34,
        _ => 100,
    }
}

/// Whether a representation is text the daemon may read for previews and
/// search. Anything else is opaque bytes it only ever copies.
pub fn is_text(mime: &str) -> bool {
    let lowered = lower(mime);
    lowered.starts_with("text/")
        || matches!(lowered.as_str(), "utf8_string" | "string" | "text")
        || lowered.starts_with("x-special/")
}

pub fn is_image(mime: &str) -> bool {
    lower(mime).starts_with("image/")
}

pub fn is_files(mime: &str) -> bool {
    matches!(
        lower(mime).as_str(),
        "x-special/gnome-copied-files" | "x-special/nautilus-clipboard" | "text/uri-list"
    )
}

/// Derives the kind from what an entry holds.
///
/// `sample` is the text of the entry's best text representation, already
/// truncated; it is what separates a bare link or a colour from prose.
pub fn classify<'a>(mimes: impl IntoIterator<Item = &'a str>, sample: Option<&str>) -> Kind {
    let mut image = false;
    let mut files = false;
    for mime in mimes {
        image |= is_image(mime);
        files |= is_files(mime);
    }
    // Files first: a file manager offers a thumbnail alongside the reference,
    // and the reference is the thing worth pasting.
    if files {
        return Kind::Files;
    }
    if image {
        return Kind::Image;
    }

    match sample.map(str::trim) {
        Some(text) if is_color(text) => Kind::Color,
        Some(text) if is_link(text) => Kind::Link,
        _ => Kind::Text,
    }
}

/// A single URL and nothing else. Prose that merely contains a link stays
/// prose, because the useful distinction is "this entry is a link".
fn is_link(text: &str) -> bool {
    if text.is_empty() || text.split_whitespace().count() != 1 {
        return false;
    }
    let lowered = text.to_ascii_lowercase();
    [
        "https://", "http://", "ftp://", "ssh://", "mailto:", "magnet:",
    ]
    .iter()
    .any(|scheme| lowered.starts_with(scheme) && lowered.len() > scheme.len())
}

/// `#rgb`, `#rrggbb`, `#rrggbbaa`, or a `rgb(…)`/`hsl(…)` function.
fn is_color(text: &str) -> bool {
    if let Some(digits) = text.strip_prefix('#') {
        return matches!(digits.len(), 3 | 4 | 6 | 8)
            && digits.bytes().all(|b| b.is_ascii_hexdigit());
    }
    let lowered = text.to_ascii_lowercase();
    ["rgb(", "rgba(", "hsl(", "hsla("]
        .iter()
        .any(|prefix| lowered.starts_with(prefix) && lowered.ends_with(')'))
}

/// How much of a text representation is worth showing in a list row.
const PREVIEW_CHARS: usize = 160;

/// Collapses a text representation into one line a row can show.
///
/// Runs of whitespace become single spaces so that indented code does not draw
/// as a mostly empty row, and the result is cut on a character boundary so a
/// multi-byte character is never split.
pub fn preview(text: &str) -> String {
    let mut out = String::with_capacity(PREVIEW_CHARS);
    let mut pending_space = false;
    let mut taken = 0;

    for character in text.chars() {
        if character.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            taken += 1;
            pending_space = false;
        }
        if taken >= PREVIEW_CHARS {
            out.push('…');
            break;
        }
        out.push(character);
        taken += 1;
    }

    out
}

fn lower(mime: &str) -> String {
    mime.trim().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_the_hints_password_managers_actually_set() {
        assert!(is_sensitive("x-kde-passwordManagerHint"));
        assert!(is_sensitive("X-KDE-PasswordManagerHint"));
        assert!(is_sensitive("application/x-nspasteboard-concealed-type"));
        assert!(!is_sensitive("text/plain"));
        assert!(any_sensitive(["text/plain", "x-kde-passwordManagerHint"]));
        assert!(!any_sensitive(["text/plain", "text/html"]));
    }

    #[test]
    fn prefers_the_representation_that_loses_the_least() {
        assert!(rank("image/png") < rank("text/html"));
        assert!(rank("text/html") < rank("text/plain"));
        assert!(rank("text/uri-list") < rank("text/plain"));
        // An unknown image beats any text; an unknown type beats nothing.
        assert!(rank("image/avif") < rank("text/plain"));
        assert!(rank("application/x-thing") > rank("text/plain"));
    }

    #[test]
    fn a_picture_is_an_image_and_a_file_reference_is_files() {
        assert_eq!(classify(["image/png", "text/html"], None), Kind::Image);
        assert_eq!(
            classify(["x-special/gnome-copied-files", "image/png"], None),
            Kind::Files
        );
        assert_eq!(classify(["text/uri-list"], None), Kind::Files);
    }

    #[test]
    fn separates_a_bare_link_from_prose_that_mentions_one() {
        assert_eq!(
            classify(["text/plain"], Some("https://example.com")),
            Kind::Link
        );
        assert_eq!(
            classify(["text/plain"], Some("  https://example.com  ")),
            Kind::Link
        );
        assert_eq!(
            classify(["text/plain"], Some("see https://example.com for more")),
            Kind::Text
        );
        // A scheme with nothing after it is not a link.
        assert_eq!(classify(["text/plain"], Some("https://")), Kind::Text);
    }

    #[test]
    fn recognises_the_colour_notations_a_designer_copies() {
        for text in [
            "#fff",
            "#FFAA33",
            "#ffaa3380",
            "rgb(1,2,3)",
            "hsla(1,2%,3%,.5)",
        ] {
            assert_eq!(classify(["text/plain"], Some(text)), Kind::Color, "{text}");
        }
        for text in ["#fffff", "#ghij", "rgb(1,2,3", "fff"] {
            assert_eq!(classify(["text/plain"], Some(text)), Kind::Text, "{text}");
        }
    }

    #[test]
    fn a_preview_is_one_line_and_never_splits_a_character() {
        assert_eq!(
            preview("  git   rebase\n  -i HEAD~3 "),
            "git rebase -i HEAD~3"
        );
        assert_eq!(preview(""), "");
        assert_eq!(preview("   \n\t "), "");

        let long: String = "é".repeat(PREVIEW_CHARS * 2);
        let cut = preview(&long);
        // Truncation is by character, so the result is still valid text and
        // carries the ellipsis that says it was cut.
        assert!(cut.ends_with('…'));
        assert_eq!(cut.chars().count(), PREVIEW_CHARS + 1);
    }
}
