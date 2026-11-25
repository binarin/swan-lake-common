//! Coloured human output, through `anstream`: colour only when the stream
//! is a terminal (or `--color always`, `CLICOLOR_FORCE`), never with
//! `NO_COLOR` or `--color never`. Without colour the escape codes are
//! stripped, so the text is the same as without styling.

use std::{fmt::Display, io::Write};

use anstream::ColorChoice;
use anstyle::{AnsiColor, Style};

/// The global `--color`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ColorWhen {
    /// On a terminal, unless NO_COLOR is set (CLICOLOR_FORCE forces it).
    Auto,
    Always,
    Never,
}

/// Sets the colour choice for every stream of the process.
pub fn init(when: ColorWhen) {
    match when {
        ColorWhen::Auto => ColorChoice::Auto,
        ColorWhen::Always => ColorChoice::Always,
        ColorWhen::Never => ColorChoice::Never,
    }
    .write_global();
}

/// Whether stderr gets colour, as the choice and the stream decide.
pub fn stderr_colored() -> bool {
    anstream::AutoStream::choice(&std::io::stderr()) != ColorChoice::Never
}

/// The `swl <cmd>:` prefix of a message.
pub const PREFIX: Style = Style::new().bold();
pub const GOOD: Style = AnsiColor::Green.on_default();
pub const WARN: Style = AnsiColor::Yellow.on_default();
pub const ERROR: Style = AnsiColor::Red.on_default().bold();
/// What waits for the owner: the one thing they have to do.
pub const OWNER: Style = AnsiColor::Magenta.on_default().bold();
/// What waits for nobody: CI, a mirror.
pub const NOBODY: Style = AnsiColor::Cyan.on_default();

/// The style of a message's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Plain,
    Good,
    Warn,
    Error,
    Owner,
    Nobody,
}

impl Tone {
    pub fn style(self) -> Style {
        match self {
            Tone::Plain => Style::new(),
            Tone::Good => GOOD,
            Tone::Warn => WARN,
            Tone::Error => ERROR,
            Tone::Owner => OWNER,
            Tone::Nobody => NOBODY,
        }
    }
}

/// `text` in `style`; the codes are stripped where there is no colour.
pub fn paint(style: Style, text: impl Display) -> String {
    let text = text.to_string();
    if style == Style::new() || text.is_empty() {
        return text;
    }
    format!("{}{text}{}", style.render(), style.render_reset())
}

fn line(prefix: &str, tone: Tone, msg: impl Display) -> String {
    format!("{}: {}", paint(PREFIX, prefix), paint(tone.style(), msg))
}

/// `<prefix>: <msg>` on stdout, the message in `tone`.
pub fn say(prefix: &str, tone: Tone, msg: impl Display) {
    let _ = writeln!(anstream::stdout(), "{}", line(prefix, tone, msg));
}

/// `<prefix>: <msg>` on stderr, the message in `tone`.
pub fn tell(prefix: &str, tone: Tone, msg: impl Display) {
    let _ = writeln!(anstream::stderr(), "{}", line(prefix, tone, msg));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn painted_text_strips_to_the_plain_text() {
        let text = line("swl land", Tone::Owner, "waits for the owner: merge x#1");
        assert_ne!(text, "swl land: waits for the owner: merge x#1");
        let mut plain = anstream::StripStream::new(Vec::new());
        write!(plain, "{text}").unwrap();
        assert_eq!(
            String::from_utf8(plain.into_inner()).unwrap(),
            "swl land: waits for the owner: merge x#1"
        );
        assert_eq!(paint(Style::new(), "x"), "x");
        assert_eq!(paint(PREFIX, ""), "");
    }
}
