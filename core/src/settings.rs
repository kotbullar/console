//! Radio Settings: the radio-wide settings record (LINK-3's `what` 1: ferrite-app's `Settings` in its `Persist`
//! layout, as `link.rs`'s `settings_record` reads it), shown, changed and checked by ferrite's own code.
//!
//! Nothing about the menu is copied here. A scratch radio (ferrite-app's `App` over [`RadioSide`]) holds the record
//! and answers for it:
//! - the tree, names and help lines are the menu's own lists (`screens::groups`, through `Group::items`);
//! - each value is what the terminal's `get` shows (the menu's `wired` value);
//! - each setting's choices are found by turning the knob on its menu row, as on the radio (`change`), and its kind
//!   by what the stepping does: round and round (a choice), stopping at the ends (a level), or not stepping but
//!   typed (`set` takes a typed value: frequencies, the capcode, the fixed position);
//! - a change is the terminal's `set` (TERM-12: the knob walked to the value), so its checks are the radio's;
//! - a setting is in the record when changing the record's bytes changes what it shows; one the link never
//!   changes (LINK-5: region, Extend TX, PIN, cable port, KISS port) is found by writing it through ferrite's own
//!   link session (`keep_protected` keeps the radio's) and shows read only.
//!
//! The few text fields the menu edits on their own screens (the callsign, the power-on text, the owner card,
//! SETUP-11) are set on `Settings` directly with the radio's character rules.

use crate::answers::status_text;
use crate::plan::{hex, unhex, Op, Problem};
use crate::radioside::RadioSide;
use crate::records::{begin_payload, write_payloads, Reader};
use ferrite_app::persist::{Persist, Sink, Source};
use ferrite_app::screens::groups::Group;
use ferrite_app::screens::nav::Dest;
use ferrite_app::settings::Settings;
use ferrite_app::term::cmd::{self, Done};
use ferrite_app::Screen;
use ferrite_core::keys::Key;
use ferrite_core::link::{msg, what};
use ferrite_core::msg::Device;
use ferrite_ui::text::Buf;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};

/// The backup file's format name.
pub const BACKUP_FORMAT: &str = "ferrite-console-settings";
/// The note on a setting the link never writes (LINK-5).
pub const RADIO_ONLY: &str = "changes only on the radio";
/// The most values a knob walk collects before the setting counts as typed (a frequency in 6.25 kHz steps).
const MANY: usize = 64;
/// What `set` is given to make it list a setting's values (or say why it can't).
pub(crate) const NOT_A_VALUE: &str = "~~~";

struct Bytes(Vec<u8>);

impl Sink for Bytes {
    fn put(&mut self, b: &[u8]) {
        self.0.extend_from_slice(b);
    }
}

struct Rd<'a> {
    b: &'a [u8],
    n: usize,
}

impl Source for Rd<'_> {
    fn take(&mut self, out: &mut [u8]) -> bool {
        match self.b.get(self.n..self.n + out.len()) {
            Some(s) => {
                out.copy_from_slice(s);
                self.n += out.len();
                true
            }
            None => false,
        }
    }
}

/// The record as the link reads it (`settings_record`): the PIN and its counters as zeros (LINK-7).
pub fn record_of(s: &Settings) -> Vec<u8> {
    let m = Settings { pin: 0, pin_misses: 0, pin_wait: 0, ..*s };
    let mut b = Bytes(Vec::new());
    m.save(&mut b);
    b.0
}

/// The settings a record holds, over the defaults (a short record leaves the rest at them, as the radio loads one).
pub fn settings_of(rec: &[u8]) -> Settings {
    let mut s = Settings::DEFAULT;
    s.load(&mut Rd { b: rec, n: 0 });
    s
}

/// CHAN-17: the home channel a record names, as the channel number people see (1-1000), or None.
pub fn home_channel(rec: &[u8]) -> Option<u16> {
    settings_of(rec).home_ch.map(|n| n + 1)
}

/// CHAN-17 (decision 347): the record with its home channel set (1-1000 as people see it, or none), for the
/// settings write that replaces the terminal's `home` line.
pub fn with_home(rec: &[u8], home: Option<u16>) -> Vec<u8> {
    let mut s = settings_of(rec);
    s.home_ch = home.filter(|n| *n >= 1).map(|n| n - 1);
    record_of(&s)
}

/// The record's length in this Console's ferrite (the radio's INFO says its own: LINK-21, version-tied).
pub fn record_len() -> usize {
    record_of(&Settings::DEFAULT).len()
}

/// How a setting is edited.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// One of `choices`, round and round as the knob goes (a dropdown).
    Pick,
    /// One of `choices` in order, the knob stopping at the ends (a stepper).
    Level,
    /// Typed: `hint` says what it takes.
    Text,
    /// Shown, not changed here: `note` says why.
    Readonly,
}

/// One setting as the radio's menu shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    /// The menu number ("0 7 1"), or a text field's name ("owner.name").
    pub id: String,
    /// The menu number shown ("0 7 1"); empty for a field with no menu number of its own.
    pub path: String,
    pub name: String,
    /// The third-level list it is in ("Sounds"), empty for a group's own row.
    pub sub: String,
    pub help: String,
    /// What the radio shows.
    pub value: String,
    /// Not its default (MENU-12's dot).
    pub changed: bool,
    pub kind: Kind,
    pub choices: Vec<String>,
    pub hint: String,
    pub note: String,
}

/// A settings group (MENU 0 n).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupView {
    /// "0 7"; "owner" for the owner card.
    pub path: String,
    pub name: String,
    pub items: Vec<Item>,
    /// What of the group isn't in this record.
    pub note: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct View {
    pub groups: Vec<GroupView>,
    /// Notes on the record as a whole (a length this Console doesn't know).
    pub notes: Vec<String>,
}

/// A text field the menu edits on a screen of its own: its id, name, help, where it goes, and its rules.
struct TextField {
    id: &'static str,
    name: &'static str,
    help: &'static str,
    get: fn(&Settings) -> &[u8],
    set: fn(&mut Settings, &[u8]),
    max: usize,
    /// Callsign rules (SETUP-12: capitals, digits and /) instead of printable ASCII.
    call: bool,
}

const CALLSIGN: TextField = TextField {
    id: "0 8 6 1",
    name: "Callsign",
    help: "",
    get: |s| s.callsign.as_bytes(),
    set: |s, v| s.set_callsign(v),
    max: 10,
    call: true,
};
const PON_TEXT: TextField = TextField {
    id: "pon_text",
    name: "Power-on text",
    help: "Shown at power on when the message is Text.",
    get: |s| s.pon_text.as_bytes(),
    set: |s, v| s.pon_text.set(v),
    max: 16,
    call: false,
};
/// The owner card (SETUP-11): entered on the radio in setup, then edited here.
const OWNER: [TextField; 3] = [
    TextField {
        id: "owner.name",
        name: "Name",
        help: "On the owner card (boot screen).",
        get: |s| s.owner[0].as_bytes(),
        set: |s, v| s.owner[0].set(v),
        max: 20,
        call: false,
    },
    TextField {
        id: "owner.phone",
        name: "Phone",
        help: "On the owner card.",
        get: |s| s.owner[1].as_bytes(),
        set: |s, v| s.owner[1].set(v),
        max: 20,
        call: false,
    },
    TextField {
        id: "owner.email",
        name: "E-mail",
        help: "On the owner card.",
        get: |s| s.owner[2].as_bytes(),
        set: |s, v| s.owner[2].set(v),
        max: 20,
        call: false,
    },
];

fn text_field(id: &str) -> Option<&'static TextField> {
    [&CALLSIGN, &PON_TEXT, &OWNER[0], &OWNER[1], &OWNER[2]].into_iter().find(|f| f.id == id)
}

impl TextField {
    fn hint(&self) -> String {
        if self.call {
            format!("Capitals, digits and /; up to {}", self.max)
        } else {
            format!("Up to {} characters", self.max)
        }
    }

    fn check(&self, v: &str) -> Result<Vec<u8>, String> {
        let v = v.trim();
        let b: Vec<u8> = if self.call { v.to_ascii_uppercase().into_bytes() } else { v.as_bytes().to_vec() };
        if b.len() > self.max {
            return Err(format!("{}: up to {} characters", self.name, self.max));
        }
        let ok = |c: &u8| {
            if self.call {
                c.is_ascii_uppercase() || c.is_ascii_digit() || *c == b'/'
            } else {
                (0x20..0x7F).contains(c)
            }
        };
        if !b.iter().all(ok) {
            return Err(if self.call {
                format!("{}: capitals, digits and / only", self.name)
            } else {
                format!("{}: printable ASCII only", self.name)
            });
        }
        Ok(b)
    }

    fn item(&self, s: &Settings, path: &str, sub: &str) -> Item {
        let v = String::from_utf8_lossy((self.get)(s)).into_owned();
        Item {
            id: self.id.into(),
            path: path.into(),
            name: self.name.into(),
            sub: sub.into(),
            help: self.help.into(),
            changed: (self.get)(s) != (self.get)(&Settings::DEFAULT),
            value: v,
            kind: Kind::Text,
            choices: Vec::new(),
            hint: self.hint(),
            note: String::new(),
        }
    }
}

/// "0 7 1".
pub fn path_text(p: &[u8]) -> String {
    let mut s = String::from("0");
    for x in p {
        s.push_str(&format!(" {x}"));
    }
    s
}

/// "0 7 1" back to [7, 1].
pub(crate) fn parse_path(id: &str) -> Option<Vec<u8>> {
    let mut w = id.split_whitespace();
    (w.next()? == "0").then_some(())?;
    let p: Vec<u8> = w.map(|x| x.parse().ok()).collect::<Option<_>>()?;
    (2..=3).contains(&p.len()).then_some(p)
}

/// A menu path's name, if it names an item.
pub(crate) fn path_name(p: &[u8]) -> Option<&'static str> {
    match *p {
        [g, m] if (1..=9).contains(&g) => items(g, 0).get((m as usize).checked_sub(1)?).map(|e| e.name()),
        [g, m, i] if (1..=9).contains(&g) => items(g, m).get((i as usize).checked_sub(1)?).map(|e| e.name()),
        _ => None,
    }
}

fn items(g: u8, sub: u8) -> &'static [ferrite_app::screens::groups::Entry] {
    Group { g, sub, row: 0 }.items()
}

/// A menu item's help line: in ferrite's text pack (ferrite UPD-22), which this host build of ferrite-app carries.
fn help_text(e: &ferrite_app::screens::groups::Entry) -> String {
    e.help().map(|t| ferrite_app::text::load(t).as_str().to_string()).unwrap_or_default()
}

/// The menu's leaves (settings and pages), in menu order: path, name, help, and the third-level list's name.
pub(crate) fn leaves() -> Vec<(Vec<u8>, &'static str, String, &'static str)> {
    let mut out = Vec::new();
    for g in 1..=9u8 {
        for (i, e) in items(g, 0).iter().enumerate() {
            let m = i as u8 + 1;
            let sub = items(g, m);
            if sub.is_empty() {
                out.push((vec![g, m], e.name(), help_text(e), ""));
            } else {
                for (j, s) in sub.iter().enumerate() {
                    out.push((vec![g, m, j as u8 + 1], s.name(), help_text(s), e.name()));
                }
            }
        }
    }
    out
}

/// A group's name as its screen heads itself.
fn group_name(g: u8) -> String {
    let mut b = Buf::<32>::new();
    Dest::Group(g).heading(&mut b);
    String::from_utf8_lossy(b.as_bytes()).into_owned()
}

/// The values of the settings no record carries (by id, "0 5 6 4"), as the terminal's `dump` writes them.
pub type Extras = BTreeMap<String, String>;

/// Where the dump's settings end (its next section, TERM-13): Console reads the dump only this far.
pub const DUMP_SETTINGS_END: &str = "# Zones";
/// The note on a terminal-backed setting not read from the radio (a backup file without it, an older firmware).
pub const NOT_READ: &str = "not read from the radio";

/// A setting the radio's menu changes that neither the settings record (LINK-3) nor the APRS record (LINK-43)
/// carries (Watch, Battery Save, the message sound, and MENU 0 5's symbol, comment, ambiguity, Mic-E, telemetry, TX
/// delay and level): read with the terminal's `dump` and changed with its `set`, over the link (`CMD`, LINK-34).
/// Found, never listed: a menu row whose value the dump writes and neither record holds.
#[derive(Clone, Debug)]
pub struct Term {
    pub path: Vec<u8>,
    pub name: &'static str,
    /// Why the link can't change it (TX-48.2's Dry TX), or None.
    pub readonly: Option<String>,
    /// Its value on a radio with the default settings.
    pub default: String,
    /// What a typed one takes (the radio's own refusal of a value too long).
    pub hint: String,
    /// Its choices from the default on (the knob's order), for a list.
    pub order: Vec<String>,
}

/// A dump's `set` lines (TERM-13) as values by id; other lines are skipped.
pub fn dump_values(text: &str) -> Extras {
    let mut out = Extras::new();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("set 0 ") else { continue };
        let clean = |v: &str| v.rfind("  # ").map_or(v, |i| &v[..i]).trim_end().to_string();
        let n = |w: &str| w.parse::<u8>().ok();
        let w: Vec<&str> = rest.splitn(4, ' ').collect();
        if let [a, b, c, v] = w[..] {
            if let (Some(a), Some(b), Some(c)) = (n(a), n(b), n(c)) {
                if path_name(&[a, b, c]).is_some() {
                    out.insert(path_text(&[a, b, c]), clean(v));
                    continue;
                }
            }
        }
        let w: Vec<&str> = rest.splitn(3, ' ').collect();
        if let [a, b, v] = w[..] {
            if let (Some(a), Some(b)) = (n(a), n(b)) {
                if path_name(&[a, b]).is_some() {
                    out.insert(path_text(&[a, b]), clean(v));
                }
            }
        }
    }
    out
}

/// The terminal-backed settings' values from a dump the radio sent (only those: the records carry the rest).
pub fn extras_from_dump(text: &str) -> Extras {
    let ids: Vec<String> = term_items().iter().map(|t| path_text(&t.path)).collect();
    dump_values(text).into_iter().filter(|(id, _)| ids.contains(id)).collect()
}

thread_local! {
    static TERM: RefCell<Option<Vec<Term>>> = const { RefCell::new(None) };
}

/// The terminal-backed settings, in menu order: the dump's settings (ferrite's `SETTINGS` table, every row the
/// terminal's `set` takes) less those either record holds and the text fields set here by hand. Found once.
/// Never called inside `scratch` (it uses the scratch radios itself).
pub fn term_items() -> Vec<Term> {
    if let Some(v) = TERM.with(|c| c.borrow().clone()) {
        return v;
    }
    learn(&record_of(&Settings::DEFAULT));
    let aprs = crate::aprs_settings::record_paths();
    let mut sc = Scratch::new();
    let s = with_call(Settings::DEFAULT);
    sc.load(&s);
    let defaults = sc.dump_values();
    let mut out = Vec::new();
    for (p, name, _, _) in leaves() {
        let id = path_text(&p);
        if class_of(&p).is_some_and(|c| c.in_record) || aprs.contains(&p) || text_field(&id).is_some() {
            continue;
        }
        let Some(def) = defaults.get(&id) else { continue };
        sc.load(&s);
        let (kind, order, _, note) = classify(&mut sc, &s, &p, name, def);
        let mut hint = String::new();
        if kind == Kind::Text {
            sc.load(&s);
            if let Err(e) = sc.set(&p, &"~".repeat(200)) {
                hint = e.strip_prefix(&format!("{name}: ")).unwrap_or(&e).to_string();
            }
        }
        out.push(Term {
            path: p,
            name,
            readonly: (kind == Kind::Readonly).then_some(note),
            default: def.clone(),
            hint,
            order: if kind == Kind::Readonly { Vec::new() } else { order },
        });
    }
    TERM.with(|c| *c.borrow_mut() = Some(out.clone()));
    out
}

/// A terminal-backed setting as a row: its value as read, its choices from the knob on the scratch radio (which
/// holds `ex`).
pub(crate) fn term_item(sc: &mut Scratch, s: &Settings, t: &Term, ex: &Extras, sub: &str, help: &str) -> Item {
    let id = path_text(&t.path);
    let mut it = Item {
        id: id.clone(),
        path: id.clone(),
        name: t.name.into(),
        sub: sub.into(),
        help: help.into(),
        value: String::new(),
        changed: false,
        kind: Kind::Readonly,
        choices: Vec::new(),
        hint: String::new(),
        note: NOT_READ.into(),
    };
    let Some(v) = ex.get(&id) else { return it };
    it.value = v.clone();
    it.changed = *v != t.default;
    if let Some(n) = &t.readonly {
        it.note = n.clone();
        return it;
    }
    let (kind, mut choices, hint, note) = classify(sc, s, &t.path, t.name, v);
    if kind == Kind::Pick {
        if let Some(i) = t.order.first().and_then(|f| choices.iter().position(|c| c == f)) {
            choices.rotate_left(i);
        }
    }
    it.kind = kind;
    it.choices = if kind == Kind::Readonly { Vec::new() } else { choices };
    it.hint = if hint.is_empty() { t.hint.clone() } else { hint };
    it.note = note;
    it
}

/// One terminal-backed setting changed on the scratch radio with the terminal's `set` (what Console then sends):
/// the record and the terminal-backed values after it, or the radio's reason.
pub(crate) fn term_set(
    sc: &mut Scratch,
    s: &Settings,
    t: &Term,
    ex: &Extras,
    value: &str,
) -> Result<Extras, String> {
    let id = path_text(&t.path);
    if let Some(n) = &t.readonly {
        return Err(format!("{}: {n}", t.name));
    }
    if !ex.contains_key(&id) {
        return Err(format!("{}: {NOT_READ}", t.name));
    }
    if value.contains('#') {
        // the terminal's line ends at a # (its note): what follows would be lost
        return Err(format!("{}: no # (the radio's command line takes the rest as a note)", t.name));
    }
    sc.load(s);
    sc.set(&t.path, value)?;
    let now = sc.dump_values();
    Ok(ex.iter().map(|(k, v)| (k.clone(), now.get(k).cloned().unwrap_or_else(|| v.clone()))).collect())
}

/// LINK-66 (decision 347): Watch (0 1 1), Battery Save (0 7 7 1) and the message sound (0 7 5 5), the settings the
/// settings record doesn't hold, as the radio record carries them: their values as the menu names them.
const WATCH_NAMES: [&str; 3] = ["Off", "A+B", "A+B+C"];
const WATCH_ID: &str = "0 1 1";
const BSAVE_ID: &str = "0 7 7 1";
const SOUND_ID: &str = "0 7 5 5";

fn bsave_name(v: u8) -> String {
    if v == 0 { "Off".into() } else { format!("Level {v}") }
}

fn bsave_value(t: &str) -> Option<u8> {
    if t == "Off" {
        return Some(0);
    }
    t.strip_prefix("Level ")?.parse().ok().filter(|v| (1..=3).contains(v))
}

/// The terminal-backed settings' values from the radio record the radio sent (LINK-66).
pub fn extras_from_radio(rec: &[u8]) -> Extras {
    let mut e = Extras::new();
    if let [w, b, _, sound, ..] = *rec {
        if let Some(n) = WATCH_NAMES.get(w as usize) {
            e.insert(WATCH_ID.into(), (*n).into());
        }
        if b <= 3 {
            e.insert(BSAVE_ID.into(), bsave_name(b));
        }
        if let Some(n) = ferrite_core::tonegen::CHIME_NAMES.get(sound as usize) {
            e.insert(SOUND_ID.into(), (*n).into());
        }
    }
    e
}

/// The radio record's write (its first four bytes) that takes a radio from `base` to `want`, if any value differs;
/// a value not in `want` keeps `base`'s.
pub(crate) fn radio_write(base: &Extras, want: &Extras) -> Option<Vec<u8>> {
    let pick = |id: &str| want.get(id).or_else(|| base.get(id));
    if [WATCH_ID, BSAVE_ID, SOUND_ID].iter().all(|id| want.get(*id).is_none_or(|v| base.get(*id) == Some(v))) {
        return None;
    }
    let w = WATCH_NAMES.iter().position(|n| Some(&n.to_string()) == pick(WATCH_ID))?;
    let b = bsave_value(pick(BSAVE_ID)?)?;
    let s = ferrite_core::tonegen::CHIME_NAMES.iter().position(|n| Some(&n.to_string()) == pick(SOUND_ID))?;
    Some(vec![w as u8, b, 0, s as u8])
}

/// The `set` lines that take a radio from `base` to `want` (the terminal-backed settings that differ).
pub(crate) fn term_cmds(base: &Extras, want: &Extras) -> Vec<String> {
    term_items()
        .iter()
        .filter(|t| t.readonly.is_none())
        .map(|t| path_text(&t.path))
        .filter_map(|id| match (base.get(&id), want.get(&id)) {
            (Some(a), Some(b)) if a != b => Some(format!("set {id} {b}")),
            _ => None,
        })
        .collect()
}

/// What `get` says for a path.
pub(crate) struct Got {
    pub(crate) value: String,
    pub(crate) changed: bool,
    pub(crate) page: bool,
}

/// A radio in memory to ask: ferrite's App, its link session and a channel store ([`RadioSide`]), driven by the
/// terminal's commands and the keys and knob as a person would.
pub(crate) struct Scratch {
    rs: RadioSide,
    dev: Device,
    t: u32,
    /// The APRS record (LINK-43) each load puts back, through ferrite's link session, for APRS settings.
    pub(crate) aprs: Option<Vec<u8>>,
    /// The settings in neither record ([`Term`]) each load puts back with the terminal's `set`: the radio's
    /// values, one missing from them at its default.
    pub(crate) term: Option<(Vec<Term>, Extras)>,
}

impl Scratch {
    pub(crate) fn new() -> Self {
        Scratch {
            rs: RadioSide::new(),
            dev: Device { tx_built: true, ..Device::default() },
            t: 1000,
            aprs: None,
            term: None,
        }
    }

    /// Every setting's value as the terminal's `dump` writes it (TERM-13: `set` takes it back as written), by id.
    pub(crate) fn dump_values(&mut self) -> Extras {
        let mut pos = cmd::DumpPos::default();
        let mut l = cmd::Line::new();
        let mut text = String::new();
        while cmd::dump_line(&self.rs.app, &self.rs.store, &self.dev, &mut pos, &mut l) {
            let line = String::from_utf8_lossy(l.as_bytes());
            if line.starts_with(DUMP_SETTINGS_END) {
                break;
            }
            text.push_str(&line);
            text.push('\n');
        }
        dump_values(&text)
    }

    /// The terminal-backed settings put back as the radio holds them (at each load: a walk or a `set` may have
    /// changed one).
    fn put_term(&mut self) {
        let Some((items, ex)) = self.term.clone() else { return };
        let now = self.dump_values();
        for t in items.iter().filter(|t| t.readonly.is_none()) {
            let id = path_text(&t.path);
            let want = ex.get(&id).unwrap_or(&t.default);
            if now.get(&id) != Some(want) {
                let _ = self.set(&t.path, want);
            }
        }
    }

    /// A line sent as Console sends it (`CMD`, LINK-34, through ferrite's link session, `MORE` pages and all): the
    /// text, or the refusal.
    pub(crate) fn link_cmd(&mut self, line: &str) -> Result<String, String> {
        let (s, rest) = self.rs.ask(msg::CMD, line.as_bytes());
        let r = if s != 0 {
            Err(status_text(s, &rest))
        } else {
            let mut out = String::new();
            let mut rest = rest;
            loop {
                let more = rest.first().copied().unwrap_or(0) != 0;
                out.push_str(&String::from_utf8_lossy(rest.get(1..).unwrap_or_default()));
                if !more {
                    break;
                }
                let (s, r) = self.rs.ask(msg::MORE, &[]);
                if s != 0 {
                    break;
                }
                rest = r;
            }
            Ok(out)
        };
        self.drain();
        r
    }

    fn drain(&mut self) {
        while self.rs.app.pop_cmd().is_some() {}
        while let Some(w) = self.rs.app.pop_write() {
            self.rs.store.apply(w);
        }
    }

    pub(crate) fn load(&mut self, s: &Settings) {
        *self.rs.app.settings_mut() = *s;
        self.rs.app.go_home();
        self.drain();
        if let Some(a) = self.aprs.clone() {
            let _ = self.write_record(what::APRS, &a);
        }
        self.put_term();
    }

    /// A record written through ferrite's link session (`BEGIN`, the `WRITE` parts, `END`): the refusal, if any.
    pub(crate) fn write_record(&mut self, w: u8, rec: &[u8]) -> Result<(), String> {
        let mut err = None;
        let (s, rest) = self.rs.ask(msg::BEGIN, &begin_payload(1));
        if s != 0 {
            err = Some(status_text(s, &rest));
        }
        if err.is_none() {
            for p in write_payloads(w, 0, rec) {
                let (s, rest) = self.rs.ask(msg::WRITE, &p);
                if s != 0 {
                    err = Some(status_text(s, &rest));
                    break;
                }
            }
        }
        self.rs.ask(msg::END, &[]);
        self.drain();
        err.map_or(Ok(()), Err)
    }

    /// A record read through ferrite's link session.
    pub(crate) fn read_record(&mut self, w: u8) -> Result<Vec<u8>, String> {
        let mut r = Reader::new(w, 0);
        while !r.done() {
            let (s, rest) = self.rs.ask(msg::READ, &r.next_payload());
            if s != 0 || !r.take(&rest) {
                return Err(format!("the read back: {}", status_text(s, &rest)));
            }
        }
        Ok(r.record)
    }

    /// For the menu: channel-only mode (PIN-5) keeps MENU 0 shut, so it is off while the knob walks.
    pub(crate) fn load_for_menu(&mut self, s: &Settings) {
        self.load(&Settings { chan_only: false, ..*s });
    }

    pub(crate) fn settings(&self) -> Settings {
        *self.rs.app.settings()
    }

    /// One terminal line: what it said, or its refusal.
    pub(crate) fn run(&mut self, line: &str) -> Result<String, String> {
        let r = cmd::run(&mut self.rs.app, &self.rs.store, &self.dev, line.as_bytes(), false);
        self.drain();
        match r {
            Ok(Done::Said(m)) => Ok(String::from_utf8_lossy(m.as_bytes()).into_owned()),
            Ok(_) => Ok(String::new()),
            Err(m) => Err(String::from_utf8_lossy(m.as_bytes()).into_owned()),
        }
    }

    pub(crate) fn get(&mut self, p: &[u8], name: &str) -> Got {
        let line = self.run(&format!("get {}", path_text(p))).unwrap_or_default();
        let head = format!("{} {}: ", path_text(p), name);
        let mut v = line.strip_prefix(&head).unwrap_or(&line).to_string();
        let page = v == "a page on the radio, not a setting";
        let changed = v.ends_with(" (not the default)");
        if changed {
            v.truncate(v.len() - " (not the default)".len());
        }
        // The terminal's answers write a byte outside ASCII as `?` (TONE-5's "120\u{b0}", WATCH-17.3's
        // "A\u{b7}B first"); its dump writes a setting's value as it is.
        let v = if v.contains('?') {
            self.dump_values().remove(&path_text(p)).unwrap_or_else(|| v.replace("??", "\u{b0}"))
        } else {
            v
        };
        Got { value: if page { String::new() } else { v }, changed, page }
    }

    /// The knob on the setting's menu row until it shows `want` (up, then down for a level that stopped): the
    /// settings then, or None if the menu never shows it.
    pub(crate) fn step_to(&mut self, s: &Settings, p: &[u8], name: &str, want: &str) -> Option<Settings> {
        self.load_for_menu(s);
        if !self.open_row(p) {
            return None;
        }
        let start = self.get(p, name).value;
        for d in [1, -1] {
            for _ in 0..MANY {
                let v = self.get(p, name).value;
                if v == want {
                    return Some(Settings { chan_only: s.chan_only, ..self.settings() });
                }
                self.knob(d);
                let w = self.get(p, name).value;
                if w == v || w == start {
                    break;
                }
            }
        }
        None
    }

    /// `set`, from home (TERM-17 refuses a setting open on the menu).
    pub(crate) fn set(&mut self, p: &[u8], value: &str) -> Result<String, String> {
        self.rs.app.go_home();
        self.run(&format!("set {} {}", path_text(p), value))
    }

    fn tap(&mut self, k: Key) {
        self.rs.app.key_down(k, self.t, &self.rs.store);
        self.t += 20;
        self.rs.app.key_up(k, self.t, &self.rs.store);
        self.t += 20;
        self.drain();
    }

    pub(crate) fn knob(&mut self, d: i8) {
        self.rs.app.knob(d, self.t, &self.rs.store);
        self.t += 20;
        self.drain();
    }

    /// The menu open on this setting's row (MENU 0 g, or MENU 0 g m), as ▼ gets there.
    pub(crate) fn open_row(&mut self, p: &[u8]) -> bool {
        self.rs.app.go_home();
        let (dest, row) = match *p {
            [g, m] => (Dest::Group(g), m - 1),
            [g, m, i] => (Dest::Sub(g, m), i - 1),
            _ => return false,
        };
        self.rs.app.open(dest);
        self.drain();
        for _ in 0..row {
            self.tap(Key::Down);
        }
        matches!(self.rs.app.screen(), Screen::Group(gr) if gr.row == row)
    }

    /// The values the knob steps through on this row, and whether it stops at its ends (a level). A level comes
    /// lowest first; a round list from where the knob was.
    pub(crate) fn walk(&mut self, s: &Settings, p: &[u8], name: &str) -> (Vec<String>, bool) {
        self.load_for_menu(s);
        let start = self.get(p, name).value;
        if !self.open_row(p) {
            return (vec![start], false);
        }
        let mut seen = vec![start.clone()];
        let mut level = false;
        for _ in 0..MANY {
            self.knob(1);
            let v = self.get(p, name).value;
            if &v == seen.last().unwrap() {
                level = true;
                break;
            }
            if v == start {
                break;
            }
            seen.push(v);
        }
        if level {
            let mut down = vec![seen.last().unwrap().clone()];
            for _ in 0..MANY {
                self.knob(-1);
                let v = self.get(p, name).value;
                if &v == down.last().unwrap() {
                    break;
                }
                down.push(v);
            }
            down.reverse();
            seen = down;
        }
        (seen, level)
    }

    /// The record after a write through ferrite's link session (`apply`, with `keep_protected`), the radio holding
    /// `old` before it; or the refusal. `BEGIN` .. `END` as Console sends it.
    fn through_link(&mut self, old: &Settings, rec: &[u8]) -> Result<Vec<u8>, String> {
        self.load(old);
        let mut err = None;
        let (s, rest) = self.rs.ask(msg::BEGIN, &begin_payload(1));
        if s != 0 {
            err = Some(status_text(s, &rest));
        }
        if err.is_none() {
            for p in write_payloads(what::SETTINGS, 0, rec) {
                let (s, rest) = self.rs.ask(msg::WRITE, &p);
                if s != 0 {
                    err = Some(status_text(s, &rest));
                    break;
                }
            }
        }
        self.rs.ask(msg::END, &[]);
        self.drain();
        if let Some(e) = err {
            return Err(e);
        }
        let mut r = Reader::new(what::SETTINGS, 0);
        while !r.done() {
            let (s, rest) = self.rs.ask(msg::READ, &r.next_payload());
            if s != 0 || !r.take(&rest) {
                return Err(format!("the read back: {}", status_text(s, &rest)));
            }
        }
        Ok(r.record)
    }
}

/// What the scratch radios found about each setting, which doesn't depend on the record: found once.
#[derive(Clone)]
struct Class {
    /// What shows depends on the record.
    in_record: bool,
    /// The first of its choices on a record of zeros (the list's own first), for ordering a round list.
    first: Option<String>,
    /// The link writes it (LINK-5), when it is a choice.
    linked: Option<bool>,
}

thread_local! {
    static SCRATCH: RefCell<Option<Scratch>> = const { RefCell::new(None) };
    static LINKSIDE: RefCell<Option<Scratch>> = const { RefCell::new(None) };
    static CLASS: RefCell<HashMap<Vec<u8>, Class>> = RefCell::new(HashMap::new());
}

fn scratch<R>(f: impl FnOnce(&mut Scratch) -> R) -> R {
    SCRATCH.with(|c| f(c.borrow_mut().get_or_insert_with(Scratch::new)))
}

fn linkside<R>(f: impl FnOnce(&mut Scratch) -> R) -> R {
    LINKSIDE.with(|c| f(c.borrow_mut().get_or_insert_with(Scratch::new)))
}

/// Whether what the path shows follows the record: the record with each bit k flipped in every byte, for k in 0..8.
fn in_record(sc: &mut Scratch, rec: &[u8], p: &[u8], name: &str) -> bool {
    sc.load(&settings_of(rec));
    let base = sc.get(p, name).value;
    (0..8).any(|k| {
        let flipped: Vec<u8> = rec.iter().map(|b| b ^ (1 << k)).collect();
        sc.load(&settings_of(&flipped));
        sc.get(p, name).value != base
    })
}

/// A callsign for walks: CW ID (0 8 6 2) won't step without one (VOX-10).
pub(crate) fn with_call(mut s: Settings) -> Settings {
    if s.callsign.is_empty() {
        s.set_callsign(b"N0CALL");
    }
    s
}

/// How a setting (in the record) is changed here, found with `set` and the knob.
pub(crate) fn classify(
    sc: &mut Scratch,
    s: &Settings,
    p: &[u8],
    name: &str,
    cur: &str,
) -> (Kind, Vec<String>, String, String) {
    // With a callsign: CW ID (0 8 6 2) is a choice even before one is set (VOX-10's refusal comes at the change).
    sc.load(&with_call(*s));
    let strip = |e: &str| e.strip_prefix(&format!("{name}: ")).unwrap_or(e).to_string();
    match sc.set(p, NOT_A_VALUE) {
        Ok(_) => (Kind::Text, Vec::new(), String::new(), String::new()),
        Err(e) if e.contains(RADIO_ONLY) => (Kind::Readonly, Vec::new(), String::new(), RADIO_ONLY.into()),
        Err(e) if e.contains("can't be set yet") => (Kind::Readonly, Vec::new(), String::new(), RADIO_ONLY.into()),
        Err(e) if e.contains(": one of ") => {
            let (mut v, level) = sc.walk(&with_call(*s), p, name);
            if v.len() >= MANY {
                return (Kind::Text, Vec::new(), String::new(), String::new());
            }
            let first = class_of(p).and_then(|c| c.first);
            if !level {
                if let Some(i) = first.and_then(|f| v.iter().position(|x| *x == f)) {
                    v.rotate_left(i);
                }
            }
            (if level { Kind::Level } else { Kind::Pick }, v, String::new(), String::new())
        }
        Err(e) => {
            sc.load(s);
            match sc.set(p, cur) {
                Ok(_) => (Kind::Text, Vec::new(), strip(&e), String::new()),
                Err(why) => (Kind::Readonly, Vec::new(), String::new(), strip(&why)),
            }
        }
    }
}

fn class_of(p: &[u8]) -> Option<Class> {
    CLASS.with(|c| c.borrow().get(p).cloned())
}

/// Finds each leaf's [`Class`] once (on `rec`, any record of this layout).
fn learn(rec: &[u8]) {
    if CLASS.with(|c| !c.borrow().is_empty()) {
        return;
    }
    let zeros = vec![0u8; rec.len()];
    let zero = with_call(settings_of(&zeros));
    let s = settings_of(rec);
    let mut map = HashMap::new();
    scratch(|sc| {
        for (p, name, _, _) in leaves() {
            let mut in_rec = in_record(sc, rec, &p, name);
            if in_rec {
                // Shown from the record but kept elsewhere (APRS's frequency follows the region): the knob changes
                // what shows and not the record.
                sc.load_for_menu(&with_call(s));
                let (r0, v0) = (record_of(&sc.settings()), sc.get(&p, name).value);
                if sc.open_row(&p) {
                    sc.knob(1);
                    if sc.get(&p, name).value != v0 && record_of(&sc.settings()) == r0 {
                        in_rec = false;
                    }
                }
            }
            let mut c = Class { in_record: in_rec, first: None, linked: None };
            if in_rec {
                sc.load(&zero);
                if let Err(e) = sc.set(&p, NOT_A_VALUE) {
                    if e.contains(": one of ") {
                        c.first = sc.walk(&zero, &p, name).0.first().cloned();
                        // LINK-5: another value, written through ferrite's link session from this record.
                        let (vals, _) = sc.walk(&with_call(s), &p, name);
                        sc.load(&with_call(s));
                        let cur = sc.get(&p, name).value;
                        if let Some(alt) = vals.iter().find(|v| **v != cur) {
                            if let Some(n) = sc.step_to(&with_call(s), &p, name, alt) {
                                let want = record_of(&n);
                                let old = with_call(s);
                                let back = linkside(|l| l.through_link(&old, &want));
                                c.linked = Some(back.is_ok_and(|b| {
                                    sc.load(&settings_of(&b));
                                    sc.get(&p, name).value == *alt
                                }));
                            }
                        }
                    }
                }
            }
            map.insert(p, c);
        }
    });
    CLASS.with(|c| *c.borrow_mut() = map);
}

/// The record (and the terminal-backed settings' values, `ex`) as the radio's menu shows them, grouped as MENU 0
/// groups them. MENU 0 5's terminal-backed settings are APRS Settings' ([`crate::aprs_settings`]).
pub fn view(rec: &[u8], ex: &Extras) -> View {
    learn(rec);
    let term = term_items();
    let aprs = crate::aprs_settings::record_paths();
    let s = settings_of(rec);
    let mut notes = Vec::new();
    if rec.len() != record_len() {
        notes.push(format!(
            "This radio's settings record is {} bytes; this Console's ferrite has {}: read only until both are the same version.",
            rec.len(),
            record_len()
        ));
    }
    let mut groups: Vec<GroupView> = (1..=9)
        .map(|g| GroupView { path: format!("0 {g}"), name: group_name(g), items: Vec::new(), note: String::new() })
        .collect();
    let mut radio_only: Vec<Vec<&str>> = vec![Vec::new(); 9];
    let mut on_aprs: Vec<&str> = Vec::new();
    scratch(|sc| {
        sc.term = Some((term.clone(), ex.clone()));
        for (p, name, help, sub) in leaves() {
            let g = p[0] as usize - 1;
            let id = path_text(&p);
            if id == CALLSIGN.id {
                groups[g].items.push(CALLSIGN.item(&s, &id, sub));
                continue;
            }
            if let Some(t) = term.iter().find(|t| t.path == p) {
                if p[0] == 5 {
                    on_aprs.push(name);
                } else {
                    groups[g].items.push(term_item(sc, &s, t, ex, sub, &help));
                }
                continue;
            }
            sc.load(&s);
            let got = sc.get(&p, name);
            if got.page {
                continue;
            }
            let class = class_of(&p).unwrap_or(Class { in_record: false, first: None, linked: None });
            if !class.in_record {
                if aprs.contains(&p) {
                    on_aprs.push(name);
                } else if !got.value.is_empty() {
                    radio_only[g].push(name);
                }
                continue;
            }
            let (mut kind, choices, hint, mut note) = classify(sc, &s, &p, name, &got.value);
            if class.linked == Some(false) {
                (kind, note) = (Kind::Readonly, RADIO_ONLY.into());
            }
            groups[g].items.push(Item {
                id: id.clone(),
                path: id,
                name: name.into(),
                sub: sub.into(),
                help,
                value: got.value,
                changed: got.changed,
                kind,
                choices: if kind == Kind::Readonly { Vec::new() } else { choices },
                hint,
                note,
            });
            if p == [7, 3, 2] {
                groups[g].items.push(PON_TEXT.item(&s, "", sub));
            }
        }
        sc.term = None;
    });
    for (g, names) in radio_only.iter().enumerate() {
        if !names.is_empty() {
            groups[g].note = format!("Shown on the radio only: {}.", names.join(", "));
        }
    }
    if !on_aprs.is_empty() {
        let n = &mut groups[4].note;
        let a = format!("On APRS Settings (Tools › APRS › APRS Settings): {}.", on_aprs.join(", "));
        *n = if n.is_empty() { a } else { format!("{a} {n}") };
    }
    groups.push(crate::crossband::group(&s)); // not in the menu tree: its start page's (MENU 0 3 4)
    groups.push(GroupView {
        path: "owner".into(),
        name: "Owner card".into(),
        items: OWNER.iter().map(|f| f.item(&s, "", "")).collect(),
        note: "Setup's page 4 (SETUP-11); shown at boot when 0 7 3 3 Boot screen is Owner card.".into(),
    });
    groups.retain(|g| !g.items.is_empty() || !g.note.is_empty());
    View { groups, notes }
}

/// One setting changed: the menu's `set` (the knob walked to it, with the radio's checks), the terminal's `set` for
/// one no record carries, or a text field set by the radio's rules. The new record and terminal-backed values, or
/// why not in the radio's words.
pub fn set(rec: &[u8], ex: &Extras, id: &str, value: &str) -> Result<(Vec<u8>, Extras), String> {
    let mut s = settings_of(rec);
    if let Some(r) = crate::crossband::set(&s, id, value) {
        return r.map(|n| (record_of(&n), ex.clone()));
    }
    if let Some(f) = text_field(id) {
        let v = f.check(value)?;
        (f.set)(&mut s, &v);
        return Ok((record_of(&s), ex.clone()));
    }
    let p = parse_path(id).ok_or_else(|| format!("{id}: not a setting"))?;
    let term = term_items();
    if let Some(t) = term.iter().find(|t| t.path == p && p[0] != 5) {
        let value = value.trim();
        return scratch(|sc| {
            sc.term = Some((term.clone(), ex.clone()));
            let r = term_set(sc, &s, t, ex, value).map(|ex| (record_of(&Settings { chan_only: s.chan_only, ..sc.settings() }), ex));
            sc.term = None;
            r
        });
    }
    if class_of(&p).is_some_and(|c| c.linked == Some(false)) {
        return Err(format!("{id}: {RADIO_ONLY}"));
    }
    let name = path_name(&p).ok_or_else(|| format!("{id}: not a setting"))?;
    let value = value.trim();
    scratch(|sc| {
        sc.term = Some((term.clone(), ex.clone()));
        let r = (|| {
            // A choice: the knob turned to it on the menu row, as on the radio (`change`).
            if class_of(&p).is_some_and(|c| c.first.is_some()) {
                return match sc.step_to(&s, &p, name, value) {
                    Some(n) => Ok(record_of(&n)),
                    None => Err(match sc.set(&p, NOT_A_VALUE) {
                        Err(e) if e.contains(": one of ") && e.contains(value) => {
                            format!("{name}: the radio won't take {value} now")
                        }
                        Err(e) => e.replace("??", "\u{b0}"),
                        Ok(_) => format!("{name}: not changed"),
                    }),
                };
            }
            // Typed (or refused): the terminal's `set`, with the radio's checks.
            sc.load(&s);
            sc.set(&p, value)?;
            Ok(record_of(&sc.settings()))
        })();
        sc.term = None;
        r.map(|r| (r, ex.clone()))
    })
}

/// A setting that differs between two records.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Change {
    pub id: String,
    pub path: String,
    pub group: String,
    pub name: String,
    pub from: String,
    pub to: String,
}

/// What differs, setting by setting, as the menu shows them; bytes no setting shows come as one "Other" line.
pub fn diff(old: &[u8], old_ex: &Extras, new: &[u8], new_ex: &Extras) -> Vec<Change> {
    let (a, b) = (view(old, old_ex), view(new, new_ex));
    let mut out = Vec::new();
    for (ga, gb) in a.groups.iter().zip(&b.groups) {
        for (x, y) in ga.items.iter().zip(&gb.items) {
            if x.id == y.id && x.value != y.value {
                out.push(Change {
                    id: y.id.clone(),
                    path: y.path.clone(),
                    group: gb.name.clone(),
                    name: y.name.clone(),
                    from: x.value.clone(),
                    to: y.value.clone(),
                });
            }
        }
    }
    if out.is_empty() && old != new {
        let n = old.iter().zip(new).filter(|(x, y)| x != y).count() + old.len().abs_diff(new.len());
        out.push(Change {
            id: "other".into(),
            path: String::new(),
            group: String::new(),
            name: "Other settings".into(),
            from: String::new(),
            to: format!("{n} bytes no menu item shows"),
        });
    }
    out
}

/// A settings write: the diff, `BEGIN`'s payload, the one write (its parts), the terminal's `set` lines for the
/// settings no record carries (sent after the write, LINK-34), and what ferrite's own link session would refuse or
/// keep (a write with problems must not be sent).
#[derive(Clone, Debug, Serialize)]
pub struct Plan {
    pub changes: Vec<Change>,
    pub begin: Vec<u8>,
    pub ops: Vec<Op>,
    pub cmds: Vec<String>,
    pub problems: Vec<Problem>,
}

/// The write from `base` (the radio's record and values, read just now) to `want`, run first through ferrite's link
/// session seeded with `base`: refusals, and any setting that wouldn't end up as asked (LINK-5's), are problems.
pub fn plan(base: &[u8], base_ex: &Extras, want: &[u8], want_ex: &Extras) -> Plan {
    let changes = diff(base, base_ex, want, want_ex);
    let mut problems = Vec::new();
    let mut ops = Vec::new();
    let problem = |e: String| Problem { what: "settings".into(), number: 0, error: e };
    if base.len() != record_len() || want.len() != record_len() {
        problems.push(problem(format!(
            "the radio's settings record is {} bytes, this Console's {}: update one so they match",
            base.len(),
            record_len()
        )));
    } else if !changes.is_empty() {
        if base != want {
            ops.push(Op {
                kind: "write",
                what: "settings",
                number: 0,
                msg: msg::WRITE,
                payloads: write_payloads(what::SETTINGS, 0, want),
            });
        }
        let page: Vec<String> =
            term_items().iter().filter(|t| t.path[0] != 5).map(|t| path_text(&t.path)).collect();
        let only = |e: &Extras| -> Extras { e.iter().filter(|(k, _)| page.contains(k)).map(|(k, v)| (k.clone(), v.clone())).collect() };
        // Checked here with `set` lines on the scratch radio; the radio itself gets the radio record (decision 347).
        let cmds = term_cmds(&only(base_ex), &only(want_ex));
        if let Some(r) = radio_write(&only(base_ex), &only(want_ex)) {
            ops.push(Op { kind: "write", what: "radio", number: 0, msg: msg::WRITE, payloads: write_payloads(what::RADIO, 0, &r) });
        }
        let term = term_items();
        let run = linkside(|l| {
            l.term = Some((term, base_ex.clone()));
            let r = (|| {
                let mut back = if ops.is_empty() { base.to_vec() } else { l.through_link(&settings_of(base), want)? };
                if !ops.is_empty() || !cmds.is_empty() {
                    if ops.is_empty() {
                        l.load(&settings_of(base));
                    }
                    for c in &cmds {
                        l.link_cmd(c).map_err(|e| format!("{c}: {e}"))?;
                    }
                    back = l.read_record(what::SETTINGS)?;
                }
                Ok::<_, String>((back, l.dump_values()))
            })();
            l.term = None;
            r
        });
        match run {
            Err(e) => problems.push(problem(e)),
            Ok((back, now)) => {
                let back_ex: Extras = want_ex.keys().filter_map(|k| now.get(k).map(|v| (k.clone(), v.clone()))).collect();
                for c in diff(want, want_ex, &back, &back_ex) {
                    problems.push(problem(format!(
                        "{} {}: the radio would keep {} ({})",
                        c.path, c.name, c.to, RADIO_ONLY
                    )));
                }
            }
        }
    }
    Plan { changes, begin: begin_payload(ops.len() as u16), ops, cmds: Vec::new(), problems }
}

/// The backup file: Console's own JSON, the record as the link carries it (hex), the terminal-backed settings'
/// values, and the settings as the menu shows them beside them for reading.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Backup {
    pub format: String,
    pub version: u32,
    pub saved: String,
    #[serde(default)]
    pub radio: BTreeMap<String, String>,
    pub record: String,
    /// The settings no record carries, as the terminal's `dump` writes them (absent in a file before them).
    #[serde(default)]
    pub extras: Extras,
    #[serde(default)]
    pub settings: BTreeMap<String, String>,
}

/// Only this page's terminal-backed values (MENU 0 5's are APRS Settings').
fn own(ex: &Extras, aprs: bool) -> Extras {
    ex.iter().filter(|(k, _)| k.starts_with("0 5 ") == aprs).map(|(k, v)| (k.clone(), v.clone())).collect()
}

pub fn backup(rec: &[u8], ex: &Extras, saved: &str, radio: BTreeMap<String, String>) -> String {
    let settings = view(rec, ex)
        .groups
        .iter()
        .flat_map(|g| {
            g.items
                .iter()
                .map(|i| (format!("{} {}", if i.path.is_empty() { &i.id } else { &i.path }, i.name), i.value.clone()))
        })
        .collect();
    let b = Backup {
        format: BACKUP_FORMAT.into(),
        version: 1,
        saved: saved.into(),
        radio,
        record: hex(rec),
        extras: own(ex, false),
        settings,
    };
    serde_json::to_string_pretty(&b).unwrap_or_default()
}

/// The PIN lock's settings (MENU 0 9 3: PIN lock, channel-only mode, the PIN; and the PIN's counters).
const PIN_MENU: &str = "0 9 3 ";

/// The record with the PIN lock's fields cleared (mode, PIN, length, counters, channel-only): what a `.ferrite`
/// codeplug carries. Codeplugs get shared; the PIN lock stays the radio's own.
pub fn without_pin(rec: &[u8]) -> Vec<u8> {
    keep_pin(rec, &record_of(&Settings::DEFAULT))
}

/// `rec` with the PIN lock's fields taken from `from` (the radio's own record): opening a codeplug keeps them.
pub fn keep_pin(rec: &[u8], from: &[u8]) -> Vec<u8> {
    let f = settings_of(from);
    let s = settings_of(rec);
    record_of(&Settings {
        pin_mode: f.pin_mode,
        pin: f.pin,
        pin_len: f.pin_len,
        pin_misses: f.pin_misses,
        pin_wait: f.pin_wait,
        chan_only: f.chan_only,
        ..s
    })
}

/// The `.ferrite` codeplug's settings.json: the backup without the PIN lock (its record fields cleared, its menu
/// entries left out: `docs/dev/codeplug-format.md`).
pub fn codeplug_part(rec: &[u8], ex: &Extras, saved: &str, radio: BTreeMap<String, String>) -> String {
    let rec = without_pin(rec);
    let ex: Extras = ex.iter().filter(|(k, _)| !k.starts_with(PIN_MENU)).map(|(k, v)| (k.clone(), v.clone())).collect();
    let mut b: Backup = serde_json::from_str(&backup(&rec, &ex, saved, radio)).expect("our own backup");
    b.settings.retain(|k, _| !k.starts_with(PIN_MENU));
    serde_json::to_string_pretty(&b).unwrap_or_default()
}

/// A backup file back to its record (and terminal-backed values), or why not.
pub fn restore(text: &str) -> Result<(Vec<u8>, Backup), String> {
    let mut b: Backup = serde_json::from_str(text).map_err(|e| format!("not a Ferrite Console settings backup: {e}"))?;
    if b.format != BACKUP_FORMAT {
        return Err(format!("not a settings backup (format {:?})", b.format));
    }
    if b.version != 1 {
        return Err(format!("backup version {} is newer than this Console reads", b.version));
    }
    b.extras = own(&b.extras, false);
    let rec = unhex(&b.record).ok_or("its record isn't hex")?;
    Ok((rec, b))
}


/// The terminal-backed values a radio holds, read as Console reads them: `CMD dump`, its `MORE` pages until the
/// settings end.
#[cfg(test)]
pub(crate) fn read_extras(r: &mut RadioSide) -> Extras {
    // As Console reads them since decision 347: the radio record (LINK-66), not the dump.
    let (s, rest) = r.ask(msg::READ, &[what::RADIO, 0, 0, 0, 0, 200]);
    assert_eq!(s, 0, "{}", status_text(s, &rest));
    extras_from_radio(&rest[2..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::records::read_payload;

    /// A default radio's terminal-backed values.
    fn ex0() -> Extras {
        read_extras(&mut RadioSide::new())
    }

    // The record-only forms the tests below were written for: the terminal-backed values a default radio's.
    fn view(rec: &[u8]) -> View {
        super::view(rec, &ex0())
    }
    fn set(rec: &[u8], id: &str, value: &str) -> Result<Vec<u8>, String> {
        super::set(rec, &ex0(), id, value).map(|r| r.0)
    }
    fn diff(old: &[u8], new: &[u8]) -> Vec<Change> {
        super::diff(old, &ex0(), new, &ex0())
    }
    /// CHAN-17: the home channel read from the record, numbered as people see it.
    #[test]
    fn home_channel_from_the_record() {
        assert_eq!(home_channel(&record_of(&Settings::DEFAULT)), None);
        let s = Settings { home_ch: Some(10), ..Settings::DEFAULT };
        assert_eq!(home_channel(&record_of(&s)), Some(11));
    }

    fn plan(base: &[u8], want: &[u8]) -> Plan {
        super::plan(base, &ex0(), want, &ex0())
    }
    fn backup(rec: &[u8], saved: &str, radio: BTreeMap<String, String>) -> String {
        super::backup(rec, &ex0(), saved, radio)
    }

    /// A radio's record read over ferrite's link session (READ, part by part).
    fn read_over_link(r: &mut RadioSide) -> Vec<u8> {
        let mut rd = Reader::new(what::SETTINGS, 0);
        while !rd.done() {
            let (s, rest) = r.ask(msg::READ, &rd.next_payload());
            assert_eq!(s, 0, "{}", status_text(s, &rest));
            assert!(rd.take(&rest));
        }
        rd.record
    }

    fn radio_with(s: Settings) -> RadioSide {
        let mut r = RadioSide::new();
        *r.app.settings_mut() = s;
        r
    }

    fn item<'a>(v: &'a View, id: &str) -> &'a Item {
        v.groups.iter().flat_map(|g| &g.items).find(|i| i.id == id).unwrap_or_else(|| panic!("{id} not in the view"))
    }

    fn changed_radio() -> Settings {
        let mut s = Settings::DEFAULT;
        s.set_callsign(b"n0call");
        s.theme = 1;
        s.key_beep = false;
        s.region = 1;
        s.pin_len = 4;
        s.pin = 1234;
        s.mic_gain = 7;
        s
    }

    /// The record read from ferrite's link session is the one `record_of` makes (the PIN as zeros), and it
    /// decodes back to the radio's settings.
    #[test]
    fn record_matches_ferrites_session() {
        let s = changed_radio();
        let mut r = radio_with(s);
        let rec = read_over_link(&mut r);
        assert_eq!(rec, record_of(&s));
        assert_eq!(rec.len(), record_len());
        assert_eq!(settings_of(&rec), Settings { pin: 0, ..s });
        // INFO's length is the same (LINK-21).
        let (st, info) = r.ask(msg::INFO, &[]);
        assert_eq!(st, 0);
        assert_eq!(crate::answers::Info::parse(&info).unwrap().settings_len as usize, record_len());
        let (st, _) = r.ask(msg::READ, &read_payload(what::SETTINGS, 1, 0, 10));
        assert_ne!(st, 0, "one settings record only");
    }

    /// The view: the menu's numbers, names and values; choices from the knob; LINK-5's read only.
    #[test]
    fn view_is_the_menus() {
        let rec = record_of(&changed_radio());
        let v = view(&rec);
        let names: Vec<&str> = v.groups.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(
            &names[..],
            [
                "Watch and scan",
                "Channels",
                "Tools",
                "Decoders and alerts",
                "APRS",
                "GPS and time",
                "Display, sound, power",
                "Transmit",
                "System",
                "Crossband repeater",
                "Owner card"
            ]
        );
        let theme = item(&v, "0 7 1");
        assert_eq!((theme.name.as_str(), theme.value.as_str(), theme.kind), ("Theme", "Daylight", Kind::Pick));
        assert_eq!(theme.choices, ["Dark", "Daylight", "Night red", "Cyberpunk"]);
        assert!(theme.changed);
        let beep = item(&v, "0 7 5 1");
        assert_eq!((beep.name.as_str(), beep.sub.as_str(), beep.value.as_str()), ("Key beep", "Sounds", "Off"));
        assert_eq!(beep.choices, ["Off", "On"]);
        let mic = item(&v, "0 8 3");
        assert_eq!((mic.kind, mic.value.as_str()), (Kind::Level, "7"));
        assert_eq!(mic.choices, ["1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12"]);
        let tot = item(&v, "0 8 1");
        assert_eq!(tot.kind, Kind::Level);
        assert_eq!((tot.choices.first().unwrap().as_str(), tot.choices.last().unwrap().as_str()), ("30 s", "600 s"));
        // LINK-5: never changed over the link.
        for id in ["0 9 4", "0 8 8 4", "0 9 3 1", "0 9 2 3", "0 5 8"] {
            let i = item(&v, id);
            assert_eq!((i.kind, i.note.as_str()), (Kind::Readonly, RADIO_ONLY), "{id}");
        }
        assert_eq!(item(&v, "0 9 4").value, "United States");
        // PWR-24.1 (decision 328): Low Power Mode and its Auto level go over the link now.
        for id in ["0 7 7 2", "0 7 7 3"] {
            let i = item(&v, id);
            assert!(i.kind != Kind::Readonly, "{id}: {:?} {}", i.kind, i.note);
        }
        let lpm = set(&rec, "0 7 7 3", "30 %").unwrap();
        assert_eq!(settings_of(&lpm).lpm_below, 30);
        // Typed: the pager frequency.
        assert_eq!(item(&v, "0 4 2 2").kind, Kind::Text);
        let call = item(&v, "0 8 6 1");
        assert_eq!((call.kind, call.value.as_str()), (Kind::Text, "N0CALL"));
        // Not here: Dry TX (this power-on, not a setting), APRS Settings' (its record's and MENU 0 5's
        // terminal-backed ones).
        for id in ["0 8 8 5", "0 5 5 1", "0 5 4", "0 5 6 4", "0 5 5 3"] {
            assert!(v.groups.iter().flat_map(|g| &g.items).all(|i| i.id != id), "{id}");
        }
        let aprs = &v.groups[4];
        assert!(aprs.note.contains("APRS Settings") && aprs.note.contains("SSID"), "{}", aprs.note);
        assert!(aprs.note.contains("Telemetry") && aprs.note.contains("Mic-E"), "{}", aprs.note);
        // In neither record, changed with the terminal's `set` (Watch, Battery Save, the message sound).
        let watch = item(&v, "0 1 1");
        assert_eq!((watch.kind, watch.value.as_str()), (Kind::Pick, "A+B"));
        assert!(watch.choices.contains(&"Off".to_string()));
        assert_eq!(item(&v, "0 7 7 1").kind, Kind::Pick);
        // WATCH-17.3's middle dot and TONE-5's degree sign as the radio writes them (the terminal's answers can't).
        assert_eq!(item(&v, "0 5 9 7").choices, ["APRS first", "A\u{b7}B first"]);
        assert!(item(&v, "0 8 8 3").choices.contains(&"120\u{b0}".to_string()));
        // CW ID is a choice before a callsign is set (the refusal comes at the change).
        let mut no_call = changed_radio();
        no_call.callsign = Settings::DEFAULT.callsign;
        let nv = view(&record_of(&no_call));
        assert_eq!(item(&nv, "0 8 6 2").kind, Kind::Pick);
        assert!(set(&record_of(&no_call), "0 8 6 2", "On unkey").unwrap_err().contains("callsign"));
        assert!(item(&v, "0 5 3").kind == Kind::Pick, "APRS mode is in the record");
        assert!(v.notes.is_empty());
    }

    /// A change: the menu's own `set`, ferrite's refusal for a value it doesn't have, text rules.
    #[test]
    fn set_and_diff() {
        let rec = record_of(&changed_radio());
        let new = set(&rec, "0 7 1", "Cyberpunk").unwrap();
        assert_eq!(settings_of(&new).theme, 3);
        let new = set(&new, "0 7 5 1", "On").unwrap();
        let d = diff(&rec, &new);
        let got: Vec<(&str, &str, &str)> =
            d.iter().map(|c| (c.path.as_str(), c.from.as_str(), c.to.as_str())).collect();
        assert_eq!(got, [("0 7 1", "Daylight", "Cyberpunk"), ("0 7 5 1", "Off", "On")]);
        // DISP-23: night red keeps its own brightness, and the menu shows it: the diff says what 0 7 4 2 will show.
        let night = set(&rec, "0 7 1", "Night red").unwrap();
        assert!(diff(&rec, &night).iter().any(|c| c.path == "0 7 4 2" && c.to == "3"));
        let e = set(&rec, "0 7 1", "Purple").unwrap_err();
        assert!(e.starts_with("Theme: one of ") && e.contains("Night red"), "{e}");
        assert!(set(&rec, "0 9 4", "Canada").unwrap_err().contains(RADIO_ONLY));
        assert!(set(&rec, "0 9 2 3", "Terminal").unwrap_err().contains(RADIO_ONLY));
        let new = set(&rec, "0 4 2 2", "439.9875").unwrap();
        assert_eq!(settings_of(&new).pocsag_freq, 43_998_750);
        assert!(set(&rec, "0 4 2 2", "nonsense").is_err());
        let new = set(&rec, "0 8 6 1", "w1aw/7").unwrap();
        assert_eq!(settings_of(&new).callsign.as_bytes(), b"W1AW/7");
        assert!(set(&rec, "0 8 6 1", "W1AW-7").is_err());
        let new = set(&rec, "owner.name", "Alex").unwrap();
        assert_eq!(settings_of(&new).owner[0].as_bytes(), b"Alex");
        assert!(set(&rec, "owner.name", "x".repeat(21).as_str()).is_err());
        assert!(diff(&rec, &rec).is_empty());
        // Channel-only mode on: still changed (the menu walk runs with it off, and it stays on).
        let mut co = settings_of(&rec);
        co.chan_only = true;
        let new = set(&record_of(&co), "0 7 1", "Dark").unwrap();
        assert_eq!((settings_of(&new).theme, settings_of(&new).chan_only), (0, true));
        // A value with the degree sign, stepped to on the menu.
        let new = set(&rec, "0 8 8 3", "180\u{b0}").unwrap();
        assert_eq!(settings_of(&new).tail_phase, 2);
        assert_eq!(item(&view(&new), "0 8 8 3").value, "180\u{b0}");
    }

    /// A changed setting survives the write through ferrite's link session and reads back; the plan finds no
    /// problems; a write the radio would partly keep (a LINK-5 setting in the record) is a problem.
    #[test]
    fn write_and_read_back() {
        let s = changed_radio();
        let mut radio = radio_with(s);
        let base = read_over_link(&mut radio);
        let want = set(&set(&base, "0 7 1", "Cyberpunk").unwrap(), "0 7 5 1", "On").unwrap();
        let p = plan(&base, &want);
        assert!(p.problems.is_empty(), "{:?}", p.problems);
        assert_eq!(p.changes.len(), 2);
        assert_eq!(p.ops.len(), 1);
        // The plan's requests, sent to the radio as Console sends them.
        assert_eq!(radio.ask(msg::BEGIN, &p.begin).0, 0);
        for op in &p.ops {
            for part in &op.payloads {
                let (st, rest) = radio.ask(op.msg, part);
                assert_eq!(st, 0, "{}", status_text(st, &rest));
            }
        }
        assert_eq!(radio.ask(msg::END, &[]).0, 0);
        let back = read_over_link(&mut radio);
        assert_eq!(back, want);
        assert_eq!(radio.app.settings().theme, 3);
        assert!(radio.app.settings().key_beep);
        assert_eq!(radio.app.settings().pin, 1234, "the PIN stays the radio's (LINK-7)");
        // Put back.
        let p = plan(&back, &base);
        assert!(p.problems.is_empty());
        // LINK-5: a record that changes the region (a restored file from elsewhere) would be kept by the radio.
        let mut other = settings_of(&base);
        other.region = 4;
        let p = plan(&base, &record_of(&other));
        assert_eq!(p.problems.len(), 1, "{:?}", p.problems);
        assert!(p.problems[0].error.contains("0 9 4 Region"), "{}", p.problems[0].error);
        // Nothing to write.
        assert!(plan(&base, &base).ops.is_empty());
    }

    #[test]
    fn backup_round_trip() {
        let rec = record_of(&changed_radio());
        let text = backup(&rec, "2026-10-20T12:00:00Z", BTreeMap::from([("callsign".into(), "N0CALL".into())]));
        let (back, b) = restore(&text).unwrap();
        assert_eq!(back, rec);
        assert_eq!(b.settings.get("0 7 1 Theme").map(String::as_str), Some("Daylight"));
        assert!(restore("{}").is_err());
        assert!(restore(&text.replace(BACKUP_FORMAT, "ferrite-console-channels")).is_err());
    }

    /// A record of another length (another firmware) shows, but isn't written.
    #[test]
    fn other_length_read_only() {
        let mut rec = record_of(&Settings::DEFAULT);
        rec.push(0);
        assert_eq!(view(&rec).notes.len(), 1);
        let want = set(&rec, "0 7 1", "Daylight").unwrap();
        assert!(!plan(&rec, &want).problems.is_empty());
    }

    /// A setting the settings record doesn't carry (Battery Save): changed as the terminal's `set` on the scratch
    /// radio, written as the radio record (LINK-66, decision 347), read back from it; the settings record isn't
    /// written for it.
    #[test]
    fn term_setting_write_and_read_back() {
        let s = changed_radio();
        let mut radio = radio_with(s);
        let base = read_over_link(&mut radio);
        let bex = read_extras(&mut radio);
        assert_eq!(bex.get("0 7 7 1").map(String::as_str), Some("Level 1"));
        let (want, wex) = super::set(&base, &bex, "0 7 7 1", "Level 3").unwrap();
        assert_eq!(want, base);
        assert_eq!(item(&super::view(&want, &wex), "0 7 7 1").value, "Level 3");
        assert!(super::set(&base, &bex, "0 7 7 1", "Level 9").is_err());
        let p = super::plan(&base, &bex, &want, &wex);
        assert!(p.problems.is_empty(), "{:?}", p.problems);
        assert_eq!(p.ops.iter().map(|o| o.what).collect::<Vec<_>>(), ["radio"], "the radio record alone (decision 347)");
        assert!(p.cmds.is_empty());
        assert_eq!(p.changes.len(), 1);
        for op in &p.ops {
            for pl in &op.payloads {
                let (st, rest) = radio.ask(op.msg, pl);
                assert_eq!(st, 0, "{}", status_text(st, &rest));
            }
        }
        assert_eq!(read_extras(&mut radio), wex);
        assert_eq!(read_over_link(&mut radio), want);
        // Both together: a record change and a terminal-backed one.
        let (w2, x2) = super::set(&want, &wex, "0 7 1", "Cyberpunk").unwrap();
        let (w2, x2) = super::set(&w2, &x2, "0 1 1", "Off").unwrap();
        let p = super::plan(&want, &wex, &w2, &x2);
        assert!(p.problems.is_empty(), "{:?}", p.problems);
        assert_eq!((p.ops.len(), p.cmds.len(), p.changes.len()), (2, 0, 2), "the settings record and the radio record");
        // The backup keeps the values; a file without them (before them) restores the record alone.
        let text = super::backup(&w2, &x2, "2026-10-27T12:00:00Z", BTreeMap::new());
        let (r, b) = restore(&text).unwrap();
        assert_eq!((r, b.extras), (w2.clone(), own(&x2, false)), "this page's values (MENU 0 5's are APRS Settings')");
        let old: serde_json::Value = serde_json::from_str(&text).unwrap();
        let mut old = old.as_object().unwrap().clone();
        old.remove("extras");
        assert!(restore(&serde_json::Value::Object(old).to_string()).unwrap().1.extras.is_empty());
        // Not read (an older firmware's dump without it): shown, read only, never sent.
        let v = super::view(&base, &Extras::new());
        assert_eq!((item(&v, "0 7 7 1").kind, item(&v, "0 7 7 1").note.as_str()), (Kind::Readonly, NOT_READ));
        assert!(super::set(&base, &Extras::new(), "0 7 7 1", "Off").is_err());
    }

    /// The drift check: every setting the radio's menu has (ferrite's `SETTINGS` table, as the terminal's `dump`
    /// writes it) is a row of Console's Radio Settings or APRS Settings, editable there exactly when the link may
    /// change it (its `set` over `CMD` taken); and every row Console shows is a menu setting (or one of the text
    /// fields set by hand). Every other menu item is a page or named in its group's note. A setting added to the
    /// radio without Console support fails here.
    #[test]
    fn every_menu_setting_is_in_console() {
        let mut s = changed_radio();
        s.region = 1; // WX-3: weather alerts aren't greyed
        let mut radio = radio_with(s);
        let rec = read_over_link(&mut radio);
        let ex = read_extras(&mut radio);
        // The radio's own list: the dump's settings, over the link.
        let (st, mut rest) = radio.ask(msg::CMD, b"dump");
        assert_eq!(st, 0);
        let mut text = String::new();
        loop {
            text.push_str(&String::from_utf8_lossy(&rest[1..]));
            if rest[0] == 0 || text.contains(DUMP_SETTINGS_END) {
                break;
            }
            rest = radio.ask(msg::MORE, &[]).1;
        }
        let radio_list = dump_values(&text);
        assert!(radio_list.len() > 80, "the dump's settings: {}", radio_list.len());
        // Console's: both pages.
        let v = super::view(&rec, &ex);
        let aprs_rec = {
            let mut r = Reader::new(what::APRS, 0);
            while !r.done() {
                let (_, rest) = radio.ask(msg::READ, &r.next_payload());
                assert!(r.take(&rest));
            }
            r.record
        };
        let a = crate::aprs_settings::view(&rec, &aprs_rec, &ex);
        let mut console: BTreeMap<String, Item> = BTreeMap::new();
        for i in v.groups.iter().flat_map(|g| &g.items).chain(&a.items).filter(|i| !i.path.is_empty()) {
            assert!(console.insert(i.id.clone(), i.clone()).is_none(), "{} on both pages", i.id);
        }
        let missing: Vec<&String> = radio_list.keys().filter(|id| !console.contains_key(*id)).collect();
        assert!(missing.is_empty(), "menu settings Console doesn't show: {missing:?}");
        // Shown from the settings record, not in the dump (not in `SETTINGS`: their own editors on the radio): the
        // weather location codes, the fixed positions (`set` takes them typed), the callsign, Extend TX, the PIN
        // lock's three and the region (LINK-5: read only here); auto reply's page (APRS-57: its own rows, MENU 0 5 9 9).
        let extra: Vec<&String> = console.keys().filter(|id| !radio_list.contains_key(*id)).collect();
        assert_eq!(
            extra,
            ["0 4 5 2", "0 5 5 5", "0 6 9", "0 8 6 1", "0 8 8 4", "0 9 3 1", "0 9 3 2", "0 9 3 3", "0 9 4", "reply.delay", "reply.text", "reply.to"],
            "rows Console shows that aren't in the radio's settings list"
        );
        // Editable in Console exactly when the radio takes a change over the link: another of its choices (or the
        // typed value again), sent as `set` over `CMD`, then shown by the dump; a read-only one refused.
        for (id, value) in &radio_list {
            let i = &console[id];
            let mut r = radio_with(s);
            if i.kind == Kind::Readonly {
                // refused by the terminal over the link, or kept by the record's write (LINK-5: the KISS port,
                // which would take the cable from the link)
                let (st, rest) = r.ask(msg::CMD, format!("set {id} {NOT_A_VALUE}").as_bytes());
                let why = status_text(st, &rest);
                assert!(
                    st != 0 && why.contains("only on the radio") || i.note == RADIO_ONLY,
                    "{id} {}: read only here ({}), the link: {why}",
                    i.name,
                    i.note
                );
            } else {
                let to = i.choices.iter().find(|c| *c != value).unwrap_or(value);
                let (st, rest) = r.ask(msg::CMD, format!("set {id} {to}").as_bytes());
                assert_eq!(st, 0, "{id} {}: editable here, the link refuses {to}: {}", i.name, status_text(st, &rest));
                let (_, rest) = r.ask(msg::CMD, b"dump");
                let shown = rest[1..].split(|&b| b == b'\n').find_map(|l| {
                    dump_values(&String::from_utf8_lossy(l)).remove(id)
                });
                // a long dump: the line may be on a later page; then the first page's settings suffice
                if let Some(shown) = shown {
                    assert_eq!(shown, *to, "{id} {}: set over the link", i.name);
                }
            }
            // a terminal-backed one shows the dump's value (a record's shows the menu row's: "Auto: on")
            if ex.contains_key(id) {
                assert_eq!(i.value, *value, "{id} {}: the value as the radio writes it", i.name);
            }
        }
        // Every menu item: a row, a page, or named in its group's note.
        let notes: String = v.groups.iter().map(|g| g.note.as_str()).collect::<Vec<_>>().join(" ");
        let mut sc = Scratch::new();
        sc.load(&s);
        for (p, name, _, _) in leaves() {
            let id = path_text(&p);
            if console.contains_key(&id) || sc.get(&p, name).page {
                continue;
            }
            assert!(notes.contains(name), "{id} {name}: neither shown nor named in a note");
        }
    }
}
