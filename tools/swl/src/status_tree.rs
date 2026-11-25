//! The text of `swl status`: the change as a tree. Each consumer is a root;
//! under it, by input path, the rows of the shared checkouts that override
//! its inputs and of its `swl.toml` entries, a nested path
//! (`common/zwanenzang`) under its parent's row when there is one. A row
//! under several consumers is shown in full under the first; under the
//! others its line refers there. Rows under no consumer follow as roots of
//! their own.
//!
//! Each row's line has the tree glyphs and `<input path>: <name>` (the
//! name alone for a root), then aligned columns: the stage, `waits: <who>`
//! and `next: <step>`. Its detail lines follow, indented below it. The
//! glyphs are Unicode box drawing (`├─ `, `└─ `, `│  `), also without
//! colour; colour comes on a terminal only (see [`crate::style`]), by whom
//! the row waits for, with the detail lines dimmed.

use std::io::Write;

use anstyle::Style;

use crate::{
    status::{Row, Stage, Waits},
    style::{BAD, BOLD, DIM, GOOD, NOBODY, OWNER, WARN, paint},
};

/// The name column grows to fit up to this width; longer names push their
/// own line's columns.
const NAME_WIDTH: usize = 44;
/// Likewise the stage column.
const STAGE_WIDTH: usize = 26;

/// Whom or what a row waits for, for its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mood {
    Owner,
    Anyone,
    Nobody,
    /// Nothing left to do (`next: -`).
    Done,
    Unknown,
}

fn mood(row: &Row) -> Mood {
    let v = &row.verdict;
    match (&v.stage, v.waits) {
        (Stage::Unknown(_), _) => Mood::Unknown,
        _ if v.next == "-" => Mood::Done,
        (_, Waits::Owner) => Mood::Owner,
        (_, Waits::Anyone) => Mood::Anyone,
        (_, Waits::Nobody) => Mood::Nobody,
    }
}

fn mood_style(m: Mood) -> Style {
    match m {
        Mood::Owner => OWNER,
        Mood::Anyone => WARN,
        Mood::Nobody => NOBODY,
        Mood::Done => GOOD,
        Mood::Unknown => BAD,
    }
}

struct Node<'a> {
    row: &'a Row,
    /// The input path under the parent; `None` for a root.
    path: Option<&'a str>,
    /// For a row shown in full under another consumer: that consumer and
    /// the path there.
    elsewhere: Option<(&'a str, &'a str)>,
    children: Vec<Node<'a>>,
}

/// Each row's place, `(consumer, input path)`: the first of its `under`
/// whose consumer is a root row.
type Primary<'a> = Vec<Option<(&'a str, &'a str)>>;

/// The path of the row that `path` hangs below under `consumer`: its
/// nearest parent path that a row is placed at; `None` for the consumer.
fn parent_path<'a>(primary: &Primary, consumer: &str, path: &'a str) -> Option<&'a str> {
    let mut at = path;
    while let Some((up, _)) = at.rsplit_once('/') {
        if primary.contains(&Some((consumer, up))) {
            return Some(up);
        }
        at = up;
    }
    None
}

fn children<'a>(
    rows: &'a [Row],
    primary: &Primary<'a>,
    consumer: &str,
    at: Option<&str>,
) -> Vec<Node<'a>> {
    let mut out: Vec<Node> = Vec::new();
    for (row, place) in rows.iter().zip(primary) {
        let Some(place) = *place else { continue };
        for (c, path) in &row.under {
            if c != consumer || parent_path(primary, consumer, path) != at {
                continue;
            }
            let full = place == (c.as_str(), path.as_str());
            out.push(Node {
                row,
                path: Some(path),
                elsewhere: (!full).then_some(place),
                children: if full {
                    children(rows, primary, consumer, Some(path))
                } else {
                    Vec::new()
                },
            });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn tree(rows: &[Row]) -> Vec<Node<'_>> {
    let roots: Vec<&str> = rows
        .iter()
        .filter(|r| r.under.is_empty())
        .map(|r| r.name.as_str())
        .collect();
    let primary: Primary = rows
        .iter()
        .map(|r| {
            r.under
                .iter()
                .find(|(c, _)| roots.contains(&c.as_str()))
                .map(|(c, p)| (c.as_str(), p.as_str()))
        })
        .collect();
    let mut out = Vec::new();
    for (row, place) in rows.iter().zip(&primary) {
        if row.under.is_empty() {
            out.push(Node {
                row,
                path: None,
                elsewhere: None,
                children: children(rows, &primary, &row.name, None),
            });
        } else if place.is_none() {
            out.push(Node {
                row,
                path: None,
                elsewhere: None,
                children: Vec::new(),
            });
        }
    }
    out
}

/// A node with the glyphs before its line and before its detail lines.
struct Flat<'a> {
    node: &'a Node<'a>,
    lead: String,
    detail_lead: String,
    /// The first line of a root after the first: a blank line before it.
    gap: bool,
}

fn flatten<'a>(node: &'a Node<'a>, lead: String, cont: &str, out: &mut Vec<Flat<'a>>) {
    let below = if node.children.is_empty() {
        "   "
    } else {
        "│  "
    };
    out.push(Flat {
        node,
        lead,
        detail_lead: format!("{cont}{below}"),
        gap: false,
    });
    let n = node.children.len();
    for (i, child) in node.children.iter().enumerate() {
        let last = i + 1 == n;
        let (branch, more) = if last {
            ("└─ ", "   ")
        } else {
            ("├─ ", "│  ")
        };
        flatten(
            child,
            format!("{cont}{branch}"),
            &format!("{cont}{more}"),
            out,
        );
    }
}

fn width(s: &str) -> usize {
    s.chars().count()
}

fn label(node: &Node) -> (String, String) {
    let name = &node.row.name;
    match node.path {
        Some(p) => (
            format!("{p}: {name}"),
            format!("{p}: {}", paint(BOLD, name)),
        ),
        None => (name.clone(), paint(BOLD, name)),
    }
}

/// The lines of the status, styled; the styling is stripped where there is
/// no colour.
pub fn lines(rows: &[Row]) -> Vec<String> {
    let roots = tree(rows);
    let mut flat = Vec::new();
    for (i, root) in roots.iter().enumerate() {
        let from = flat.len();
        flatten(root, String::new(), "", &mut flat);
        flat[from].gap = i > 0;
    }
    let name_w = flat
        .iter()
        .map(|f| width(&f.lead) + width(&label(f.node).0))
        .max()
        .unwrap_or(0)
        .min(NAME_WIDTH);
    let stage_w = flat
        .iter()
        .map(|f| width(&f.node.row.verdict.stage.to_string()))
        .max()
        .unwrap_or(0)
        .min(STAGE_WIDTH);
    let waits_w = flat
        .iter()
        .map(|f| width(&format!("waits: {}", f.node.row.verdict.waits)))
        .max()
        .unwrap_or(0);
    let pad = |w: usize, used: usize| " ".repeat(w.saturating_sub(used));
    let mut out = Vec::new();
    for f in &flat {
        let row = f.node.row;
        let v = &row.verdict;
        let mood = mood(row);
        let color = mood_style(mood);
        let (plain_label, styled_label) = label(f.node);
        let stage = v.stage.to_string();
        let waits = format!("waits: {}", v.waits);
        let next = match mood {
            Mood::Owner => paint(OWNER, &v.next),
            _ => v.next.clone(),
        };
        if f.gap {
            out.push(String::new());
        }
        out.push(format!(
            "{}{styled_label}{}  {}{}  {}{}  next: {next}",
            paint(DIM, &f.lead),
            pad(name_w, width(&f.lead) + width(&plain_label)),
            paint(color, &stage),
            pad(stage_w, width(&stage)),
            paint(color, &waits),
            pad(waits_w, width(&waits)),
        ));
        let details: Vec<String> = match f.node.elsewhere {
            Some((c, p)) => vec![format!("details under {c}: {p}")],
            None => row.details.clone(),
        };
        for d in details {
            out.push(paint(DIM, format!("{}{d}", f.detail_lead)));
        }
    }
    out
}

/// Prints the status of `rows`.
pub fn print(rows: &[Row]) {
    let mut stdout = anstream::stdout();
    for l in lines(rows) {
        let _ = writeln!(stdout, "{l}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::{RowFacts, Verdict};

    fn row(name: &str, under: &[(&str, &str)], stage: Stage, next: &str, waits: Waits) -> Row {
        Row {
            name: name.into(),
            follows: None,
            under: under
                .iter()
                .map(|(c, p)| (c.to_string(), p.to_string()))
                .collect(),
            verdict: Verdict {
                stage,
                next: next.into(),
                waits,
                landable: false,
            },
            details: vec![format!("{name} detail")],
            facts: RowFacts::None,
        }
    }

    fn plain(lines: &[String]) -> String {
        let mut out = anstream::StripStream::new(Vec::new());
        for l in lines {
            writeln!(out, "{l}").unwrap();
        }
        String::from_utf8(out.into_inner()).unwrap()
    }

    #[test]
    fn a_tree_per_consumer_with_nested_entries_and_references() {
        let rows = [
            row(
                "zwanenzang@zwanenzang",
                &[("swan-lake", "common/zwanenzang")],
                Stage::Landed,
                "-",
                Waits::Nobody,
            ),
            row(
                "swan-lake-common@common",
                &[("swan-lake", "common"), ("nixos-config", "common")],
                Stage::Merged,
                "promote swan-lake-current",
                Waits::Owner,
            ),
            row(
                "forks#3",
                &[("swan-lake", "forks")],
                Stage::Unknown("offline".into()),
                "rerun swl status",
                Waits::Nobody,
            ),
            row(
                "swan-lake",
                &[],
                Stage::Ci(crate::forge::CiState::Green),
                "wait for swan-lake-common#24 (promote swan-lake-current)",
                Waits::Owner,
            ),
            row(
                "nixos-config",
                &[],
                Stage::NoChange("HEAD is in origin/master".into()),
                "-",
                Waits::Nobody,
            ),
        ];
        let want = "\
swan-lake                                     CI green                    waits: owner   next: wait for swan-lake-common#24 (promote swan-lake-current)
│  swan-lake detail
├─ common: swan-lake-common@common            merged                      waits: owner   next: promote swan-lake-current
│  │  swan-lake-common@common detail
│  └─ common/zwanenzang: zwanenzang@zwanenzang  landed                      waits: nobody  next: -
│        zwanenzang@zwanenzang detail
└─ forks: forks#3                             unknown (offline)           waits: nobody  next: rerun swl status
      forks#3 detail

nixos-config                                  no change (HEAD is in origin/master)  waits: nobody  next: -
│  nixos-config detail
└─ common: swan-lake-common@common            merged                      waits: owner   next: promote swan-lake-current
      details under swan-lake: common
";
        assert_eq!(plain(&lines(&rows)), want);
        // Coloured by whom each row waits for, details dimmed.
        let styled = lines(&rows);
        assert!(
            styled[0].contains(&paint(OWNER, "waits: owner")),
            "{styled:?}"
        );
        assert!(styled[1].starts_with(DIM.render().to_string().as_str()));
        assert!(styled[4].contains(&paint(GOOD, "landed")), "{styled:?}");
        assert!(styled[6].contains(&paint(BAD, "unknown (offline)")));
    }

    #[test]
    fn rows_under_no_consumer_are_roots() {
        let rows = [
            row(
                "a@a",
                &[],
                Stage::NoMrNeeded("x".into()),
                "-",
                Waits::Nobody,
            ),
            row("b#1", &[("gone", "b")], Stage::Merged, "-", Waits::Nobody),
        ];
        assert_eq!(
            plain(&lines(&rows)),
            "\
a@a  no MR needed (x)  waits: nobody  next: -
   a@a detail

b#1  merged            waits: nobody  next: -
   b#1 detail
"
        );
        assert!(lines(&[]).is_empty());
    }
}
