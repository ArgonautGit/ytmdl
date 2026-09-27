//! The open-source licenses page, from Settings: ytmdl's own license (the
//! GPL's "Appropriate Legal Notices") and the notices of the third-party code
//! in the APK, which tools/gen-notices writes to app/notices.json.

use std::sync::OnceLock;

use dioxus::prelude::*;
use serde::Deserialize;

use super::views::{CENTRED, LicensePage, LicenseView, LicensesPage, NoticeGroup, NoticeRow, TextBlock};
use super::{Ctx, Overlay, dotted_text};

const GPL: &str = include_str!("../../../LICENSE");
const NOTICES: &str = include_str!("../../notices.json");
const VERSION: &str = env!("CARGO_PKG_VERSION");
const SOURCE: &str = env!("CARGO_PKG_REPOSITORY");

/// A license page.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Notice {
    /// ytmdl's own license.
    Own,
    /// An item of a group in app/notices.json: (group, item).
    Item(usize, usize),
}

#[derive(Deserialize)]
struct Notices {
    groups: Vec<Group>,
    texts: Vec<Text>,
}

#[derive(Deserialize)]
struct Group {
    title: String,
    items: Vec<Item>,
}

#[derive(Deserialize)]
struct Item {
    name: String,
    version: Option<String>,
    license: String,
    by: Option<String>,
    url: Option<String>,
    /// Indices into `texts`.
    texts: Vec<usize>,
}

#[derive(Deserialize)]
struct Text {
    license: String,
    text: String,
}

fn notices() -> &'static Notices {
    static NOTICES_JSON: OnceLock<Notices> = OnceLock::new();
    NOTICES_JSON.get_or_init(|| serde_json::from_str(NOTICES).expect("app/notices.json (tools/gen-notices)"))
}

#[component]
pub(super) fn LicensesScreen() -> Element {
    let ctx = use_context::<Ctx>();
    let groups = use_hook(groups);
    rsx! {
        LicensesPage {
            version: VERSION.to_string(),
            source: SOURCE.to_string(),
            groups,
            onback: move |_| ctx.nav.back(),
            onown: move |_| ctx.nav.push(Overlay::License(Notice::Own)),
            onopen: move |(group, item)| ctx.nav.push(Overlay::License(Notice::Item(group, item))),
        }
    }
}

#[component]
pub(super) fn LicenseScreen(notice: Notice) -> Element {
    let ctx = use_context::<Ctx>();
    let license = use_hook(move || license_view(notice));
    rsx! {
        LicensePage { license, onback: move |_| ctx.nav.back() }
    }
}

pub(super) fn groups() -> Vec<NoticeGroup> {
    notices()
        .groups
        .iter()
        .map(|g| NoticeGroup {
            title: g.title.clone(),
            rows: g
                .items
                .iter()
                .map(|i| NoticeRow {
                    title: i.name.clone(),
                    sub: dotted_text(&[i.version.clone().unwrap_or_default(), i.license.clone()]),
                })
                .collect(),
        })
        .collect()
}

pub(super) fn license_view(notice: Notice) -> LicenseView {
    let Notice::Item(group, item) = notice else {
        return LicenseView {
            title: "ytmdl".into(),
            sub: dotted_text(&[VERSION.into(), "GPL-3.0-or-later".into()]),
            by: None,
            url: Some(SOURCE.into()),
            texts: vec![(None, text_blocks(GPL))],
        };
    };
    let n = notices();
    let Some(item) = n.groups.get(group).and_then(|g| g.items.get(item)) else {
        return LicenseView::default();
    };
    let several = item.texts.len() > 1;
    LicenseView {
        title: item.name.clone(),
        sub: dotted_text(&[item.version.clone().unwrap_or_default(), item.license.clone()]),
        by: item.by.clone(),
        url: item.url.clone(),
        texts: item
            .texts
            .iter()
            .filter_map(|&t| n.texts.get(t))
            .map(|t| (several.then(|| t.license.clone()), text_blocks(&t.text)))
            .collect(),
    }
}

/// Lays a plain-text license out for a phone: lines its author wrapped are
/// joined into paragraphs (keeping their indent), a line underlined with
/// `===` or `---` becomes a heading, and tables stay as written.
pub(super) fn text_blocks(text: &str) -> Vec<TextBlock> {
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    let wrap = wrap_width(&lines);
    let mut out = Vec::new();
    for chunk in lines.split(|l| l.is_empty()).filter(|c| !c.is_empty()) {
        if chunk.iter().any(|l| l.trim_start().contains("   ")) {
            let table = chunk.join("\n");
            match out.last_mut() {
                Some(TextBlock::Pre(above)) => *above = format!("{above}\n\n{table}"),
                _ => out.push(TextBlock::Pre(table)),
            }
            continue;
        }
        // Paragraphs are wrapped at different widths; this one's is about its longest line.
        let widest = chunk.iter().map(|l| l.chars().count()).max().unwrap_or(0);
        // (indent, text, lines in it)
        let mut para: Option<(usize, String, usize)> = None;
        let mut flush = |para: Option<(usize, String, usize)>, heading: bool| match para {
            Some((_, text, 1)) if heading => out.push(TextBlock::Heading(text)),
            Some((indent, text, _)) => out.push(TextBlock::Para { indent, text }),
            None => {}
        };
        let mut previous = "";
        for line in chunk {
            let body = line.trim_start();
            let indent = line.len() - body.len();
            if is_rule(body) {
                flush(para.take(), true);
                continue;
            }
            // Wrapped (rather than broken on purpose) after a long enough
            // line that stops mid-sentence, or goes on in lowercase, or
            // whose next word didn't fit.
            let next_word = body.split_whitespace().next().map_or(0, |w| w.chars().count());
            let width = previous.chars().count();
            let fits = |limit| width + 1 + next_word <= limit;
            let capital = body.starts_with(|c: char| c.is_uppercase());
            let lower = body.starts_with(|c: char| c.is_lowercase());
            let sentence = previous.ends_with(['.', ':', '!', '?']);
            let on_purpose = sentence && (fits(widest) || (capital && fits(wrap)));
            let wrapped = width * 2 >= wrap && (lower || !on_purpose);
            previous = line;
            match &mut para {
                // A first line may be indented more than the rest; lines
                // indented far are centred, each on its own.
                Some((at, text, n))
                    if wrapped
                        && indent < CENTRED
                        && !starts_item(body)
                        && !(text.starts_with("Copyright") && capital)
                        && (indent >= *at || (*n == 1 && *at - indent <= 4)) =>
                {
                    *at = indent.min(*at);
                    text.push(' ');
                    text.push_str(body);
                    *n += 1;
                }
                _ => {
                    flush(para.take(), false);
                    para = Some((indent, body.to_string(), 1));
                }
            }
        }
        flush(para, false);
    }
    out
}

/// About the width the text was wrapped at: most of a paragraph's lines come
/// close to it.
fn wrap_width(lines: &[&str]) -> usize {
    let mut lengths: Vec<usize> = lines.iter().map(|l| l.chars().count()).filter(|&n| n > 0).collect();
    lengths.sort_unstable();
    lengths.get(lengths.len() * 9 / 10).copied().unwrap_or(80)
}

/// `=====`, `-----` and the like.
fn is_rule(line: &str) -> bool {
    let mut chars = line.chars();
    let first = chars.next();
    line.len() >= 3 && matches!(first, Some('=' | '-' | '~' | '*' | '^' | '_')) && chars.all(|c| Some(c) == first)
}

/// Whether a line starts a list item or a copyright notice, rather than
/// carrying on the line before it.
fn starts_item(line: &str) -> bool {
    if line.starts_with("Copyright") || line.starts_with('©') {
        return true;
    }
    let marker = line.split_whitespace().next().unwrap_or("");
    if matches!(marker, "-" | "*" | "•") {
        return true;
    }
    // 1.  2)  (a)  ii.  A.
    let inner = marker.trim_start_matches('(').trim_end_matches(['.', ')']);
    let n = inner.chars().count();
    inner.len() < marker.len()
        && n > 0
        && ((n <= 4 && inner.chars().all(|c| c.is_ascii_digit() || c == '.'))
            || (n <= 3 && inner.chars().all(|c| c.is_ascii_lowercase()))
            || (n == 1 && inner.chars().all(|c| c.is_ascii_uppercase())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn para(indent: usize, text: &str) -> TextBlock {
        TextBlock::Para { indent, text: text.into() }
    }

    #[test]
    fn reads_the_notices() {
        let n = notices();
        let titles: Vec<&str> = n.groups.iter().map(|g| g.title.as_str()).collect();
        assert_eq!(titles, ["Bundled software", "Android libraries", "Rust crates"]);
        for item in n.groups.iter().flat_map(|g| &g.items) {
            assert!(!item.texts.is_empty(), "{} has no license text", item.name);
            assert!(item.texts.iter().all(|&t| t < n.texts.len()), "{}", item.name);
        }
        let crates = &n.groups[2].items;
        assert!(crates.iter().any(|c| c.name == "dioxus"));
        assert!(!crates.iter().any(|c| c.name.starts_with("ytmdl")));
        let view = license_view(Notice::Item(0, 0));
        assert_eq!(view.title, "Python");
        assert!(view.texts.len() == 1 && view.texts[0].0.is_none());
        assert_eq!(license_view(Notice::Item(9, 9)), LicenseView::default());
    }

    #[test]
    fn joins_wrapped_lines() {
        let text = "\
MIT License

Copyright (c) 2015 Someone
Copyright (c) 2016 Someone Else

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the \"Software\"), to deal
in the Software without restriction.
Short line on its own.

   1. Definitions.
      (a) You must give any other recipients of the Work or Derivative Works
          a copy of this License; and
      (b) You must cause any modified files to carry prominent notices.
";
        assert_eq!(
            text_blocks(text),
            [
                para(0, "MIT License"),
                para(0, "Copyright (c) 2015 Someone"),
                para(0, "Copyright (c) 2016 Someone Else"),
                para(
                    0,
                    "Permission is hereby granted, free of charge, to any person obtaining a copy of this software \
                     and associated documentation files (the \"Software\"), to deal in the Software without restriction."
                ),
                para(0, "Short line on its own."),
                para(3, "1. Definitions."),
                para(6, "(a) You must give any other recipients of the Work or Derivative Works a copy of this License; and"),
                para(6, "(b) You must cause any modified files to carry prominent notices."),
            ]
        );
    }

    #[test]
    fn keeps_headings_and_tables() {
        let text = "\
A. HISTORY OF THE SOFTWARE
==========================

    Release         Derived     Year
    0.9.0 thru 1.2              1991-1995
";
        assert_eq!(
            text_blocks(text),
            [
                TextBlock::Heading("A. HISTORY OF THE SOFTWARE".into()),
                TextBlock::Pre("    Release         Derived     Year\n    0.9.0 thru 1.2              1991-1995".into()),
            ]
        );
        let gpl = text_blocks(GPL);
        assert_eq!(
            gpl[..4],
            [
                para(20, "GNU GENERAL PUBLIC LICENSE"),
                para(23, "Version 3, 29 June 2007"),
                para(1, "Copyright (C) 2007 Free Software Foundation, Inc. <https://fsf.org/>"),
                para(1, "Everyone is permitted to copy and distribute verbatim copies of this license document, but changing it is not allowed."),
            ]
        );
    }
}
