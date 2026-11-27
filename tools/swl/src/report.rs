//! The progress output of the multi-step commands (`land`, `bump`,
//! `promote`, `refresh`, `publish`, `finalize`): one presentation for all of
//! them, as a tree that is printed while the run goes on.
//!
//! A run's lines come in sections, one per repo, MR or phase (`common`,
//! `swan-lake#31`). A section's heading, `swl <cmd>: <title>`, is printed
//! with its first line, so a section that has nothing to say prints
//! nothing. Under it:
//!
//! - steps (what swl does, what it found) as `├─ <step>`, outcomes that
//!   stand out (pushed, promoted, merged) in their colour;
//! - details (the commands swl runs, the waits, a step's further lines)
//!   dimmed, as `│  <detail>`;
//! - the section's final outcome (what it waits for, what a dry run would
//!   do next, its result) as `└─ <outcome>`, which closes it.
//!
//! The output streams, so which line of a section is its last is not known
//! when a line is printed. A section is therefore closed by its final
//! outcome when the command knows it; one that the run leaves without (it
//! moves on to the next section, or an error ends the run) is closed with
//! [`CLOSE`] when the next line or the end of the run comes. A line that
//! repeats the section's subject (`swan-lake#31: …` under `swan-lake#31`)
//! drops it. Lines about the whole run (`nothing to bump`, `done: …`) stand
//! outside any section, prefixed `swl <cmd>:`.
//!
//! The glyphs are Unicode box drawing, also without colour (a pipe, a file,
//! `NO_COLOR`), as `status` draws its tree; colour comes through
//! [`crate::style`] and is stripped where there is none.

use std::{cell::RefCell, fmt::Display, io::Write};

use anstyle::Style;

use crate::style::{BOLD, DIM, PREFIX, Tone, WARN, paint, width};

/// A child that more lines follow at its level.
pub const BRANCH: &str = "├─ ";
/// The last child.
pub const LAST: &str = "└─ ";
/// Below a child that more lines follow at its level.
pub const RAIL: &str = "│  ";
/// Below the last child.
pub const GAP: &str = "   ";
/// Closes a section that ended without a final outcome.
pub const CLOSE: &str = "╵";

/// A list line (`up to date: a, b, …`) wraps before this width, glyphs
/// included.
const LIST_WIDTH: usize = 100;

#[derive(Debug, Clone)]
struct Section {
    title: String,
    /// Other names of the subject, dropped as the title is.
    also: Vec<String>,
}

impl Section {
    /// `msg` without a leading `<subject>: `.
    fn strip<'a>(&self, msg: &'a str) -> &'a str {
        std::iter::once(&self.title)
            .chain(&self.also)
            .find_map(|s| msg.strip_prefix(s.as_str())?.strip_prefix(": "))
            .unwrap_or(msg)
    }
}

#[derive(Debug, Default)]
struct State {
    /// The section that the next line belongs to.
    current: Option<Section>,
    /// The section whose heading is printed and that is not closed yet.
    open: Option<Section>,
}

#[derive(Debug)]
enum Sink {
    Stdout,
    #[cfg(test)]
    Lines(RefCell<Vec<String>>),
}

/// The glyph of a line, and its further lines' lead and style.
#[derive(Debug, Clone, Copy)]
enum Kind {
    /// Further lines are details.
    Step,
    /// A step whose further lines continue it.
    List,
    Detail,
    End,
}

/// The progress output of one run of `cmd`, on stdout.
#[derive(Debug)]
pub struct Report {
    cmd: &'static str,
    sink: Sink,
    state: RefCell<State>,
}

impl Report {
    pub fn new(cmd: &'static str) -> Self {
        Self {
            cmd,
            sink: Sink::Stdout,
            state: RefCell::default(),
        }
    }

    /// A report that keeps its lines, for tests.
    #[cfg(test)]
    pub fn captured(cmd: &'static str) -> Self {
        Self {
            cmd,
            sink: Sink::Lines(RefCell::default()),
            state: RefCell::default(),
        }
    }

    /// The lines so far, styled.
    #[cfg(test)]
    pub fn lines(&self) -> Vec<String> {
        match &self.sink {
            Sink::Lines(l) => l.borrow().clone(),
            Sink::Stdout => Vec::new(),
        }
    }

    fn write(&self, line: String) {
        match &self.sink {
            Sink::Stdout => {
                let _ = writeln!(anstream::stdout(), "{line}");
            }
            #[cfg(test)]
            Sink::Lines(l) => l.borrow_mut().push(line),
        }
    }

    /// The lines that follow are about `title`.
    pub fn section(&self, title: impl Into<String>) {
        self.section_also(title, &[]);
    }

    /// The lines that follow are about `title`, also called `also` in them.
    pub fn section_also(&self, title: impl Into<String>, also: &[&str]) {
        self.state.borrow_mut().current = Some(Section {
            title: title.into(),
            also: also.iter().map(|a| a.to_string()).collect(),
        });
    }

    /// The lines that follow are about the whole run.
    pub fn leave(&self) {
        self.state.borrow_mut().current = None;
    }

    /// Closes the open section, if any.
    pub fn close(&self) {
        let open = self.state.borrow_mut().open.take();
        if open.is_some() {
            self.write(paint(DIM, CLOSE));
        }
    }

    /// `<cmd>: <msg>`, outside any section.
    fn top(&self, style: Style, msg: &str) {
        self.close();
        self.write(format!(
            "{}: {}",
            paint(PREFIX, self.cmd),
            paint(style, msg)
        ));
    }

    /// The first line of `msg` with `kind`'s glyph in `style`, in the
    /// current section (its heading first when it is not open), the other
    /// lines below it. Outside any section, `msg` in `top`.
    fn emit(&self, kind: Kind, style: Style, top: Style, msg: &str) {
        let current = self.state.borrow().current.clone();
        let Some(section) = current else {
            return self.top(top, msg);
        };
        let reopen = {
            let st = self.state.borrow();
            st.open.as_ref().map(|s| &s.title) != Some(&section.title)
        };
        if reopen {
            self.close();
            self.write(format!(
                "{}: {}",
                paint(PREFIX, self.cmd),
                paint(BOLD, &section.title)
            ));
            self.state.borrow_mut().open = Some(section.clone());
        }
        let msg = section.strip(msg);
        let (glyph, lead, rest) = match kind {
            Kind::Step => (BRANCH, RAIL, DIM),
            Kind::List => (BRANCH, RAIL, Style::new()),
            Kind::Detail => (RAIL, RAIL, DIM),
            Kind::End => (LAST, GAP, top),
        };
        let mut lines = msg.lines();
        let first = lines.next().unwrap_or("");
        self.write(format!("{}{}", paint(DIM, glyph), paint(style, first)));
        for l in lines {
            let l = match kind {
                Kind::Detail => l,
                _ => l.trim_start(),
            };
            self.write(format!("{}{}", paint(DIM, lead), paint(rest, l)));
        }
        if let Kind::End = kind {
            self.state.borrow_mut().open = None;
        }
    }

    /// A step: what swl does or found. Its further lines are details.
    pub fn step(&self, msg: impl Display) {
        self.emit(Kind::Step, Style::new(), Style::new(), &msg.to_string());
    }

    /// A step that needs attention: an input left, a checkout kept.
    pub fn warn(&self, msg: impl Display) {
        self.emit(Kind::Step, WARN, WARN, &msg.to_string());
    }

    /// An outcome on the way (pushed, promoted, merged), in `tone`, bold.
    pub fn outcome(&self, tone: Tone, msg: impl Display) {
        self.emit(
            Kind::Step,
            tone.style().bold(),
            tone.style(),
            &msg.to_string(),
        );
    }

    /// A detail, dimmed: a command that swl runs, a wait.
    pub fn detail(&self, msg: impl Display) {
        self.emit(Kind::Detail, DIM, DIM, &msg.to_string());
    }

    /// `<label>: a, b, …` as a step, wrapped at the commas.
    pub fn list(&self, label: &str, items: &[&str]) {
        let label = match &self.state.borrow().current {
            Some(section) => section.strip(&format!("{label}:")).to_string(),
            None => format!("{label}:"),
        };
        let mut lines = vec![label];
        for (i, item) in items.iter().enumerate() {
            let sep = if i + 1 < items.len() { "," } else { "" };
            let last = lines.last_mut().expect("one line at least");
            let used = width(BRANCH) + width(last);
            if i > 0 && used + 1 + width(item) + width(sep) > LIST_WIDTH {
                lines.push(format!("{item}{sep}"));
            } else {
                last.push(' ');
                last.push_str(item);
                last.push_str(sep);
            }
        }
        self.emit(Kind::List, Style::new(), Style::new(), &lines.join("\n"));
    }

    /// The section's final outcome, in `tone`, bold (its further lines not);
    /// it closes the section. Outside any section, a line of the whole run,
    /// in `tone`.
    pub fn end(&self, tone: Tone, msg: impl Display) {
        self.emit(
            Kind::End,
            tone.style().bold(),
            tone.style(),
            &msg.to_string(),
        );
    }

    /// A line about the whole run, outside any section, in `tone`.
    pub fn say(&self, tone: Tone, msg: impl Display) {
        self.leave();
        self.top(tone.style(), &msg.to_string());
    }
}

impl Drop for Report {
    fn drop(&mut self) {
        self.close();
    }
}

/// The lines of a tree whose children are all known: `title`, then each
/// child as `├─ <child>`, the last as `└─ <child>`.
pub fn tree(title: impl Display, children: &[String]) -> Vec<String> {
    let mut out = vec![title.to_string()];
    for (i, c) in children.iter().enumerate() {
        let glyph = if i + 1 == children.len() {
            LAST
        } else {
            BRANCH
        };
        out.push(format!("{}{c}", paint(DIM, glyph)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::GOOD;

    fn plain(lines: &[String]) -> String {
        lines
            .iter()
            .map(|l| format!("{}\n", anstream::adapter::strip_str(l)))
            .collect()
    }

    fn report_plain(r: &Report) -> String {
        plain(&r.lines())
    }

    #[test]
    fn sections_stream_as_a_tree_closed_by_their_outcome() {
        let r = Report::captured("swl bump");
        r.section_also("common", &["swan-lake-common master"]);
        r.list("common: up to date", &["disko", "nixpkgs"]);
        r.step("common: zwanenzang 11a574392516 -> e40d4498c7c3 (the default branch)");
        r.outcome(
            Tone::Good,
            "common: pushed 8ab4ff130b05 to master: swl: bump zwanenzang",
        );
        r.step(
            "swan-lake-common master: current at 4f5d3c028d43 gets 1 commit(s):\n  8ab4ff1 swl: bump zwanenzang",
        );
        r.step("promote swan-lake-common master");
        r.detail("git -C /src push origin 8ab4ff13:refs/heads/current");
        r.detail("swan-lake-common master: https://mirror current does not have 8ab4ff130b05 yet; checking again in 10s");
        r.section("nixos-config");
        r.step("push nixos-config's bump of common to 8ab4ff130b05 as origin's swl-bump-common");
        r.end(
            Tone::Nobody,
            "waits for nobody: nixos-config#372 waits for CI, then it merges (merge when checks succeed); rerun swl bump",
        );
        r.section("swan-lake");
        r.end(Tone::Good, "swan-lake: common is at 8ab4ff130b05 already");
        r.say(
            Tone::Good,
            "done: nixos-config, swan-lake lock common at 8ab4ff130b05",
        );
        assert_eq!(
            report_plain(&r),
            "\
swl bump: common
├─ up to date: disko, nixpkgs
├─ zwanenzang 11a574392516 -> e40d4498c7c3 (the default branch)
├─ pushed 8ab4ff130b05 to master: swl: bump zwanenzang
├─ current at 4f5d3c028d43 gets 1 commit(s):
│  8ab4ff1 swl: bump zwanenzang
├─ promote swan-lake-common master
│  git -C /src push origin 8ab4ff13:refs/heads/current
│  https://mirror current does not have 8ab4ff130b05 yet; checking again in 10s
╵
swl bump: nixos-config
├─ push nixos-config's bump of common to 8ab4ff130b05 as origin's swl-bump-common
└─ waits for nobody: nixos-config#372 waits for CI, then it merges (merge when checks succeed); rerun swl bump
swl bump: swan-lake
└─ common is at 8ab4ff130b05 already
swl bump: done: nixos-config, swan-lake lock common at 8ab4ff130b05
"
        );
    }

    #[test]
    fn styled_lines_dim_glyphs_and_details_and_bold_outcomes() {
        let r = Report::captured("swl land");
        r.section("x#1");
        r.outcome(Tone::Good, "x#1 is merged");
        r.detail("git push");
        r.end(Tone::Owner, "waits for the owner: merge y#2");
        let lines = r.lines();
        assert_eq!(
            lines[0],
            format!("{}: {}", paint(PREFIX, "swl land"), paint(BOLD, "x#1"))
        );
        assert_eq!(
            lines[1],
            format!(
                "{}{}",
                paint(DIM, BRANCH),
                paint(GOOD.bold(), "x#1 is merged")
            )
        );
        assert_eq!(
            lines[2],
            format!("{}{}", paint(DIM, RAIL), paint(DIM, "git push"))
        );
        assert_eq!(
            lines[3],
            format!(
                "{}{}",
                paint(DIM, LAST),
                paint(Tone::Owner.style().bold(), "waits for the owner: merge y#2")
            )
        );
        // Plain: the same text, no escape codes.
        let text = report_plain(&r);
        assert!(!text.contains('\x1b'), "{text:?}");
        assert_eq!(
            text,
            "swl land: x#1\n├─ x#1 is merged\n│  git push\n└─ waits for the owner: merge y#2\n"
        );
    }

    #[test]
    fn a_section_reentered_continues_and_one_with_nothing_to_say_prints_nothing() {
        let r = Report::captured("swl land");
        r.section("a#1");
        r.step("mark a#1 ready");
        // The next pass: a section that says nothing, then a#1 again.
        r.section("b#2");
        r.section("a#1");
        r.outcome(Tone::Good, "a#1 is merged");
        r.section("b#2");
        r.section("c#3");
        r.end(Tone::Warn, "waits for anyone: c#3: fix it\n  a detail line");
        // A line after the end reopens its section.
        r.step("c#3: more");
        r.leave();
        r.end(Tone::Nobody, "waits for nobody: too many steps");
        assert_eq!(
            report_plain(&r),
            "\
swl land: a#1
├─ mark a#1 ready
├─ a#1 is merged
╵
swl land: c#3
└─ waits for anyone: c#3: fix it
   a detail line
swl land: c#3
├─ more
╵
swl land: waits for nobody: too many steps
"
        );
    }

    #[test]
    fn the_end_of_the_run_closes_an_open_section() {
        let r = Report::captured("swl refresh");
        r.section("a#1");
        r.step("rebase a of a#1");
        r.close();
        r.close();
        assert_eq!(
            report_plain(&r),
            "swl refresh: a#1\n├─ rebase a of a#1\n╵\n"
        );
        // Without a section, lines are the run's own, prefixed.
        let r = Report::captured("swl refresh");
        r.step("nothing to refresh");
        r.end(Tone::Plain, "dry run, nothing was written");
        assert_eq!(
            report_plain(&r),
            "swl refresh: nothing to refresh\nswl refresh: dry run, nothing was written\n"
        );
    }

    #[test]
    fn long_lists_wrap_at_the_commas() {
        let r = Report::captured("swl bump");
        r.section("common");
        let items = [
            "disko",
            "emacs-niri-awareness",
            "emacs-overlay",
            "emacs-tramp-rpc",
            "flake-parts",
            "home-manager",
            "import-tree",
            "llm-agents",
            "nixpkgs",
            "nixpkgs-unstable",
            "omnidiff",
            "sops-nix",
        ];
        r.list("common: up to date", &items);
        let text = report_plain(&r);
        assert_eq!(
            text,
            "\
swl bump: common
├─ up to date: disko, emacs-niri-awareness, emacs-overlay, emacs-tramp-rpc, flake-parts,
│  home-manager, import-tree, llm-agents, nixpkgs, nixpkgs-unstable, omnidiff, sops-nix
"
        );
        assert!(text.lines().all(|l| width(l) <= LIST_WIDTH), "{text}");
    }

    #[test]
    fn a_known_tree_ends_with_its_last_child() {
        let lines = tree("workspace /ws:", &["a  b".into(), "c  d".into()]);
        assert_eq!(plain(&lines), "workspace /ws:\n├─ a  b\n└─ c  d\n");
        assert_eq!(plain(&tree("t", &[])), "t\n");
    }
}
