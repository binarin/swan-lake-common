// host-picker: a fuzzy chooser for where to go, with each row's host as a
// colored [alias] (the same host tags as host-spaces and host-agents).
//
// Two lists, Tab switches between them:
// - spaces: every space and, under it, its tabs (Enter selects it);
// - agents: every agent, and sub-agents without a pane (Enter focuses it,
//   or peeks at a sub-agent), most urgent first.
// Typing filters fuzzily: letters in order, word starts and runs rank higher.
// Several words must each match, in any order, so "cnc umb" finds the
// cnc-umbrella space on cnc. ↑/↓ (or ctrl-p/ctrl-n) move, Enter goes, Esc closes.
// props: { mode: "spaces" | "agents", overlay }.

import { View, div, svg, image } from "gpui-kit";
import { v_flex, h_flex, InputState, Input } from "gpui-base";
import { agents, spaces, panes, terminals, focused, run, theme, settings, shared, props, closeOverlay, hc, readTextFile, appHome } from "herdr";

// ---- host tags: the same block in host-spaces and host-agents; keep them in sync ----
// A colored [alias] for the machine a space or agent runs on, read from
// <app home>/host-tags.json (re-read every 3 s, so edits show at once):
//   { "hosts": { "local": { "alias": "local", "color": "ansi2" }, "claude-nixos-config": "cnc" } }
// Keys: Herdr's machine name (`hc machine ls`), that machine's host, or "local"
// for this computer. A plain string is only the alias. Colors: a theme name
// (ansi1..ansi15, accent, ...) or #rrggbb. A machine not listed shows its own
// name, in a palette color picked from that name, so it stays the same.
// (One copy per mod: a mod can't import a file outside its own folder.)
const TAG_FILE = "host-tags.json";
const TAG_POLL = 3000;
const TAG_PALETTE = ["ansi2", "ansi3", "ansi4", "ansi5", "ansi6", "ansi1"];

class HostTags {
  constructor() {
    this.text = null;
    this.table = {};
    this.hosts = {}; // machine name -> its host
  }

  start(cx) {
    const read = (cx) => cx.spawn(async (cx) => {
      let text = null;
      try { text = await readTextFile(`${appHome()}/${TAG_FILE}`); } catch {}
      if (text === this.text) return;
      this.text = text;
      try { this.table = JSON.parse(text ?? "{}").hosts ?? {}; } catch (e) { console.log(`host tags: ${TAG_FILE}: ${e}`); }
      cx.notify();
    });
    const machines = (cx) => cx.spawn(async (cx) => {
      try {
        const r = await hc(["machine", "ls", "--json"]);
        if (r.code !== 0) return;
        const hosts = Object.fromEntries((JSON.parse(r.stdout).machines ?? []).map((m) => [m.name, m.host]));
        if (JSON.stringify(hosts) === JSON.stringify(this.hosts)) return;
        this.hosts = hosts;
        cx.notify();
      } catch {}
    });
    read(cx);
    machines(cx);
    cx.timer.every(TAG_POLL, read);
    cx.timer.every(30000, machines);
  }

  // machine: null (or "") for this computer, else Herdr's machine name.
  of(machine) {
    const name = machine || null;
    const key = name ?? "local";
    let entry = this.table[key] ?? (name && this.table[this.hosts[name]]);
    if (typeof entry === "string") entry = { alias: entry };
    let sum = 0;
    for (const ch of key) sum += ch.codePointAt(0);
    return { alias: entry?.alias ?? key, color: entry?.color ?? TAG_PALETTE[sum % TAG_PALETTE.length] };
  }

  color(t, machine) {
    const c = this.of(machine).color;
    return c.startsWith("#") ? c : (t[c] ?? t.accent);
  }

  el(t, machine, size = 11) {
    return div().flex_none().text_size(size).font_semibold().text_color(this.color(t, machine)).child(`[${this.of(machine).alias}]`);
  }
}
// ---- end host tags ----

const ROW = 32;
const MODES = ["spaces", "agents"];
const RANK = { blocked: 4, done: 3, working: 2, idle: 1 };
const MARK = { ansi6: "#22a99a", ansi3: "#e3814f", ansi4: "#5b8def", ansi5: "#8b7cf6", ansi2: "#3fae6a", ansi1: "#d9649a" };

// Fuzzy score of `q` in `text`, with the matched positions; null: no match.
function fuzzy(q, text) {
  const t = text.toLowerCase();
  let score = 0;
  let at = 0;
  let run = 0;
  const hits = [];
  for (const ch of q) {
    const i = t.indexOf(ch, at);
    if (i < 0) return null;
    const start = i === 0 || /[\s:.\-_/\[\]·›]/.test(t[i - 1]);
    run = i === at && hits.length ? run + 1 : 0;
    score += 1 + (start ? 6 : 0) + run * 3 - Math.min(i - at, 6) * 0.3;
    hits.push(i);
    at = i + 1;
  }
  return { score: score - text.length * 0.02, hits };
}

// Every word of the query in `hay` (any order); the label's matched letters to light.
function match(words, label, hay) {
  let score = 0;
  const hits = new Set();
  for (const w of words) {
    const inLabel = fuzzy(w, label);
    const anywhere = inLabel ?? fuzzy(w, hay);
    if (!anywhere) return null;
    score += inLabel ? inLabel.score + 2 : anywhere.score;
    for (const h of inLabel?.hits ?? []) hits.add(h);
  }
  return { score, hits };
}

const clean = (s) => (s ?? "").replace(/^[^\p{L}\p{N}]+/u, "").trim();

function icon(p, size, color) {
  if (!p?.icon) return null;
  return p.tint ? svg(p.icon).size(size).flex_none().text_color(color) : image(p.icon).w(size).h(size).flex_none();
}

export default class HostPicker extends View {
  init(_props, cx) {
    this.mode = MODES.includes(props()?.mode) ? props().mode : "spaces";
    this.query = "";
    this.at = null; // null: the row you are at, once the list is known
    this.first = 0;
    this.listHeight = 400;
    this.tags = new HostTags();
    this.tags.start(cx);
    this.input = InputState.new({ placeholder: this.placeholder() });
    this.input.on("change", (_e, cx) => { this.query = this.input.value(); this.at = 0; this.first = 0; cx.notify(); });
    this.input.on("submit", () => this.go(this.rows()[this.at ?? 0]));
    this.input.focus();
    cx.timer.after(600, () => this.input.focus());
    this.handle = cx.focus_handle();
  }

  placeholder() {
    return "Type to filter: host, space, tab, agent…";
  }

  close() {
    closeOverlay(props().overlay);
  }

  go(r) {
    if (!r) return;
    this.close();
    if (r.kind === "space") run("space.select", { id: r.space.id });
    else if (r.kind === "tab") run("tab.select", { id: r.tab.id });
    else if (r.kind === "agent" && r.pane) run("pane.focus", { pane: r.pane });
    else if (r.kind === "agent") run("terminal.peek", { terminal: r.address });
  }

  // Every candidate, in the order shown with no query.
  all() {
    const ps = panes();
    const ags = agents();
    if (this.mode === "spaces") {
      const out = [];
      for (const s of spaces()) {
        const tag = this.tags.of(s.machine).alias;
        out.push({ kind: "space", key: `s-${s.id}`, space: s, machine: s.machine, label: s.name,
          hay: `${tag} ${s.name} ${s.branch ?? ""}`, here: s.active, status: s.status });
        for (const tab of s.tabs ?? []) {
          const mine = ags.filter((a) => a.tab === tab.id);
          const lead = ps.find((p) => p.tab === tab.id && p.program?.agent) ?? ps.find((p) => p.tab === tab.id && p.terminal);
          const progs = [...new Set(mine.map((a) => a.program?.name).filter(Boolean))].join(" · ");
          const title = clean(lead?.title);
          out.push({ kind: "tab", key: `t-${tab.id}`, space: s, tab, machine: s.machine,
            label: `${s.name} › ${tab.name}`, short: tab.name, extra: [progs, title !== tab.name ? title : ""].filter(Boolean).join(" · "),
            hay: `${tag} ${s.name} ${tab.name} ${progs} ${title}`, here: s.active && tab.active, status: tab.status });
        }
      }
      return out;
    }
    const listed = new Set(ags.map((a) => a.address));
    const rows = ags.map((a) => {
      const machine = a.address.includes("/") ? a.address.split("/")[0] : null;
      const tag = this.tags.of(machine).alias;
      const tabLabel = /^\d+$/.test(a.tab_name) ? clean(a.title) || a.tab_name : a.tab_name;
      return { kind: "agent", key: `a-${a.address}`, address: a.address, pane: a.pane, machine, program: a.program,
        label: `${a.space_name} · ${tabLabel}`, extra: a.program?.name ?? "", status: a.status, seq: a.seq,
        hay: `${tag} ${a.space_name} ${a.tab_name} ${a.title} ${a.program?.name ?? ""} ${a.app ?? ""}`, here: a.focused };
    });
    // Sub-agents that have no pane: only a peek shows them.
    const live = terminals().filter((x) => x.state === "live");
    const byAddr = new Map(live.map((x) => [x.address, x]));
    const spaceOf = (x) => { for (let p = x; p; p = byAddr.get(p.parent)) { const a = ags.find((g) => g.address === p.parent); if (a) return a; } return null; };
    for (const k of live) {
      if (!k.parent || listed.has(k.address)) continue;
      const parent = spaceOf(k);
      if (!parent) continue;
      const machine = k.address.includes("/") ? k.address.split("/")[0] : null;
      const name = k.name || k.template || k.program?.name || k.address;
      const st = k.status === "error" ? "blocked" : k.status === "done" ? "idle" : k.status;
      rows.push({ kind: "agent", key: `a-${k.address}`, address: k.address, pane: null, machine, program: k.program, sub: true,
        label: `${parent.space_name} · ${name}`, extra: `sub-agent of ${parent.program?.name ?? parent.address}`, status: st, seq: 0,
        hay: `${this.tags.of(machine).alias} ${parent.space_name} ${name} ${k.program?.name ?? ""}`, here: false });
    }
    return rows.sort((x, y) => (RANK[y.status] ?? 0) - (RANK[x.status] ?? 0) || y.seq - x.seq);
  }

  rows() {
    const words = this.query.trim().toLowerCase().split(/\s+/).filter(Boolean);
    const all = this.all();
    if (!words.length) return all;
    return all
      .map((r) => {
        const label = `[${this.tags.of(r.machine).alias}] ${r.label}`;
        const m = match(words, label, `${label} ${r.hay}`);
        // The label as drawn starts after the tag: shift the lit letters.
        const skip = label.length - r.label.length;
        return m && { ...r, flat: true, score: m.score, hits: [...m.hits].filter((h) => h >= skip).map((h) => h - skip) };
      })
      .filter(Boolean)
      .sort((a, b) => b.score - a.score);
  }

  lit(t, text, hits, color) {
    if (!hits?.length) return div().truncate().text_color(color).child(text);
    const set = new Set(hits);
    const parts = [];
    [...text].forEach((ch, i) => {
      const on = set.has(i);
      if (parts.length && parts.at(-1)[1] === on) parts.at(-1)[0] += ch;
      else parts.push([ch, on]);
    });
    return h_flex().min_w(0).overflow_hidden().children(parts.map(([s, on]) =>
      div().flex_none().text_color(on ? t.accent : color).when(on, (d) => d.font_semibold()).child(s.replace(/ /g, "\u00a0"))));
  }

  glyph(t, st) {
    if (st === "working") return svg("mods/workspace-chrome/spinner.svg").size(12).flex_none().text_color(t.working).spin(900);
    if (st === "idle") return div().size(7).mx(2).flex_none().rounded(4).border(1.5).border_color(t.idle ?? t.dim);
    if (st === "blocked" || st === "done") return div().size(7).mx(2).flex_none().rounded(4).bg(t[st]);
    return div().size(7).mx(2).flex_none();
  }

  badge(t, st) {
    if (st === "blocked") return div().flex_none().px(6).h(18).rounded(5).bg(t.blocked + "26").text_size(10.5).font_semibold().text_color(t.blocked).flex().items_center().child("Needs you");
    if (st === "done") return div().flex_none().text_size(11).text_color(t.done).child("finished");
    return null;
  }

  row(t, r, on) {
    const indent = r.kind === "tab" && !r.flat;
    const text = indent ? r.short : r.label;
    let lead;
    if (r.kind === "space") {
      const c = (shared("space.colors") ?? {})[r.space.id] ?? "ansi6";
      const fill = r.space.name === "~" ? t.dim : c.startsWith("#") ? c : (MARK[c] ?? t[c]);
      lead = h_flex().size(18).flex_none().rounded(5).bg(fill).items_center().justify_center().text_size(10.5).font_bold().text_color("#ffffff").child([...r.space.name][0]?.toUpperCase() ?? "?");
    } else if (r.kind === "agent") lead = icon(r.program, 16, t.foreground) ?? div().size(16).flex_none();
    else lead = this.glyph(t, r.status);
    const right = r.kind === "space" ? `${r.space.tabs?.length ?? 0} tab${r.space.tabs?.length === 1 ? "" : "s"}${r.space.branch ? ` · ${r.space.branch}` : ""}` : r.extra;
    return h_flex()
      .id(`hp-${r.key}`)
      .h(ROW)
      .flex_none()
      .mx(4)
      .px(10)
      .when(indent, (el) => el.pl(38))
      .when(r.sub && !r.flat, (el) => el.pl(28))
      .gap(9)
      .rounded(7)
      .items_center()
      .cursor_pointer()
      .when(on, (el) => el.bg(t.hover))
      .hover((s) => s.bg(t.hover))
      .on_click(() => this.go(r))
      .child(lead)
      .when(!indent, (el) => el.child(this.tags.el(t, r.machine, 11.5)))
      .child(h_flex().flex_shrink(1).min_w(0).when(r.here, (el) => el.font_semibold())
        .child(this.lit(t, text, indent ? [] : r.hits, r.kind === "space" || r.here ? t.foreground : t.muted)))
      .child(div().flex_1())
      .when(r.kind !== "tab" || r.flat, (el) => el.when(r.kind !== "space", (el) => el.child(this.glyph(t, r.status))))
      .when(Boolean(this.badge(t, r.status)), (el) => el.child(this.badge(t, r.status)))
      .when(Boolean(right), (el) => el.child(div().flex_shrink(1).min_w(0).max_w(220).truncate().text_size(11).text_color(t.dim).child(right)));
  }

  render() {
    const t = theme();
    const rows = this.rows();
    if (this.at === null) this.at = Math.max(0, rows.findIndex((r) => r.here && (this.mode === "agents" || r.kind === "tab")));
    this.at = Math.max(0, Math.min(this.at, rows.length - 1));
    const fit = Math.max(1, Math.floor(this.listHeight / ROW));
    if (this.at < this.first) this.first = this.at;
    if (this.at >= this.first + fit) this.first = this.at - fit + 1;
    this.first = Math.max(0, Math.min(this.first, rows.length - fit));
    const shown = rows.slice(this.first, this.first + fit);
    const sel = rows[this.at];
    const tabsBar = h_flex().gap(4).children(MODES.map((m) =>
      div().id(`hp-mode-${m}`).px(8).h(22).flex().items_center().rounded(6).text_size(11.5)
        .text_color(m === this.mode ? t.foreground : t.dim).when(m === this.mode, (el) => el.bg(t.selected))
        .cursor_pointer().on_click((_e, cx) => this.switchMode(m, cx)).child(m)));
    return v_flex()
      .id("host-picker")
      .track_focus(this.handle)
      .size_full()
      .min_h(0)
      .p(18)
      .gap(10)
      .bg(t.popover)
      .text_size(13)
      .on_key_down((e, cx) => {
        const k = e.key;
        const c = e.modifiers.control;
        if (k === "escape") return this.close();
        if (k === "tab") this.switchMode(MODES[(MODES.indexOf(this.mode) + 1) % MODES.length], cx);
        else if (k === "down" || (c && (k === "n" || k === "j"))) this.at = Math.min(rows.length - 1, this.at + 1);
        else if (k === "up" || (c && (k === "p" || k === "k"))) this.at = Math.max(0, this.at - 1);
        else if (k === "pagedown") this.at = Math.min(rows.length - 1, this.at + fit);
        else if (k === "pageup") this.at = Math.max(0, this.at - fit);
        else return;
        cx.stop_propagation();
        cx.notify();
      })
      .child(h_flex().items_center().gap(12)
        .child(div().text_size(15).font_semibold().text_color(t.foreground).child("Go to…"))
        .child(tabsBar)
        .child(div().flex_1())
        .child(div().text_size(11).text_color(t.dim).child(`${rows.length} · Tab switch · Esc`)))
      .child(h_flex().h(36).flex_none().px(10).gap(8).items_center().rounded(9).border_1().border_color(t.accent).bg(t.background)
        .child(svg("mods/workspace-chrome/search.svg").size(15).text_color(t.dim))
        .child(Input.new(this.input).flex_1().h_full().text_size(12.8).text_color(t.foreground)))
      .child(v_flex()
        .id("hp-list")
        .flex_1()
        .min_h(0)
        .overflow_hidden()
        .on_size_change((size, cx) => {
          const h = size.height / (settings().zoom ?? 1);
          if (Math.abs(h - this.listHeight) > 1) { this.listHeight = h; cx.notify(); }
        })
        .on_scroll_wheel((e, cx) => {
          const step = e.delta.y < 0 ? 1 : e.delta.y > 0 ? -1 : 0;
          if (!step) return;
          this.first = Math.max(0, Math.min(this.first + step, rows.length - fit));
          this.at = Math.max(this.first, Math.min(this.at, this.first + fit - 1));
          cx.notify();
        })
        .when(rows.length === 0, (el) => el.child(div().px(14).py(8).text_color(t.dim).child("Nothing matches.")))
        .children(shown.map((r) => this.row(t, r, r === sel))));
  }

  switchMode(m, cx) {
    if (m === this.mode) return;
    this.mode = m;
    this.at = null;
    this.first = 0;
    cx.notify();
  }
}
