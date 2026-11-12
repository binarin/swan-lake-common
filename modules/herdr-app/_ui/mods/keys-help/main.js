// keys-help: every shortcut in one list (cloned from Herdr's `help` 0.1.14).
// Herdr's own list shows keys.json only; this adds what the tmux-prefix popup
// takes after its prefix (Ctrl+O), which lives in that mod's code, not in
// keys.json. tmux-prefix publishes its table in shared "tmux-prefix.bindings"
// ([key, label] pairs) whenever it opens, so it is current after the first use.
// Type to filter (keys or what they do), Esc closes.

import { View, div } from "gpui-kit";
import { v_flex, h_flex, InputState, Input } from "gpui-base";
import { keys, props, closeOverlay, theme, shared } from "herdr";

const GROUPS = [
  ["go to", /^mod\.open:(host-picker|picker)|^app\.palette/],
  ["spaces", /^space\.|^host-spaces\./],
  ["tabs", /^tab\./],
  ["panes", /^pane\./],
  ["agents", /^agent\./],
  ["terminal", /^terminal\.|^send:/],
  ["window", /./],
];

const NAMES = {
  "mod.open:host-picker": "go to space / tab / agent (fuzzy)",
  "mod.open:picker": "go to tab (Herdr's picker)",
  "mod.open:tmux-prefix": "tmux prefix",
  "mod.open:keys-help": "this list",
  "host-spaces.toggle": "spaces overview (every space's tabs)",
  "app.help": "Herdr's key list",
  "app.palette": "command palette (fuzzy)",
  "tmux-prefix.last-tab": "last tab (while the prefix is open)",
};

function label(action) {
  if (NAMES[action]) return NAMES[action];
  if (action.startsWith("send:")) return "type " + JSON.stringify(action.slice(5));
  const [name, arg] = action.split(":");
  const words = name.replace(/[._]/g, " ");
  return arg ? `${words} ${arg}` : words;
}

export default class KeysHelp extends View {
  init(_props, cx) {
    this.filter = "";
    this.input = InputState.new({ placeholder: "Type to filter…" });
    this.input.on("change", (_e, cx) => { this.filter = this.input.value(); cx.notify(); });
    this.input.focus();
    cx.timer.after(600, () => this.input.focus());
    this.handle = cx.focus_handle();
  }

  render() {
    const t = theme();
    const f = this.filter.toLowerCase();
    const has = (keyText, what) => !f || keyText.toLowerCase().includes(f) || what.toLowerCase().includes(f);
    const bound = keys();
    const prefixKey = bound.find((k) => k.section === "global" && k.action === "mod.open:tmux-prefix")?.keys ?? "ctrl-o";
    const prefixRows = (shared("tmux-prefix.bindings") ?? [])
      .map(([key, what]) => ({ keys: `${prefixKey}  ${key}`, what, section: "global" }))
      .filter((r) => has(r.keys, r.what));
    const all = bound.map((k) => ({ keys: k.keys, what: label(k.action), action: k.action, section: k.section }))
      .filter((k) => has(k.keys, k.what) || has(k.keys, k.action));
    const seen = new Set();
    const groups = GROUPS.map(([name, re]) => [name, all.filter((k) => !seen.has(k) && re.test(k.action) && (seen.add(k) || true))]);
    groups.splice(1, 0, [`after ${prefixKey} (tmux prefix)`, prefixRows]);
    const shown = groups.filter(([, rows]) => rows.length > 0);
    const empty = !(shared("tmux-prefix.bindings") ?? []).length;
    return v_flex()
      .id("keys-help")
      .track_focus(this.handle)
      .on_key_down((e) => {
        if (e.key === "escape") return closeOverlay(props().overlay);
      })
      .size_full()
      .min_h(0)
      .p(18)
      .gap(14)
      .bg(t.popover)
      .text_size(12)
      .child(h_flex().h(22).items_center().justify_between()
        .child(div().text_size(15).font_semibold().text_color(t.foreground).child("Keys"))
        .child(div().text_size(11).text_color(t.dim).child("Esc")))
      .child(Input.new(this.input).h(36).flex_none().px(10).rounded(9).border_1().border_color(t.accent).bg(t.background).text_size(12.8).text_color(t.foreground))
      .when(empty, (el) => el.child(div().text_size(11).text_color(t.dim).child(`The ${prefixKey} keys show here once the prefix has been opened once.`)))
      .child(v_flex().flex_1().min_h(0).overflow_y_scrollbar().py(2).gap(10)
        .children(shown.map(([name, rows]) => v_flex().gap(2)
          .child(div().text_size(11.5).font_medium().text_color(t.dim).child(name))
          .children(rows.map((k) => h_flex().h(28).px(8).rounded(7).items_center()
            .child(div().w(220).flex_none().text_color(t.accent).child(k.keys + (k.section !== "global" ? `  (${k.section})` : "")))
            .child(div().truncate().text_color(t.foreground).child(k.what)))))));
  }
}
