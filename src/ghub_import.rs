//! Reading G HUB's per-key lighting presets out of its settings, without
//! touching them.
//!
//! G HUB keeps everything in one JSON document inside
//! `%LOCALAPPDATA%\LGHUB\settings.db` (SQLite, table `data`, column `file`).
//! The database is copied to a temporary folder first, with its `-wal` and
//! `-shm` files (G HUB runs it in WAL mode, and recent changes may sit in the
//! WAL), and only the copy is opened.
//!
//! A per-key preset is `lighting_effects/<id>`: groups of keys, each a map
//! from a G HUB key code to `{"hex": "#rrggbb"}`. The codes are HID usages
//! for the main keys and consumer usages for the media keys; the G-keys,
//! the logo and the lighting key are numbered from 1.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use serde_json::Value;

use crate::keymap::{self, Key};
use crate::lighting::Rgb;

/// A per-key preset as G HUB lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preset {
    pub id: String,
    pub name: String,
}

/// A preset turned into blu-ghub's key names.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Layout {
    pub keys: BTreeMap<String, Rgb>,
    /// Entries with no G915 key, or no readable colour, as `GROUP:code`.
    pub skipped: Vec<String>,
}

/// `%LOCALAPPDATA%\LGHUB\settings.db`.
pub fn settings_path() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("LGHUB").join("settings.db"))
}

/// G HUB's settings document, read from a copy of `db`.
pub fn read(db: &Path) -> Result<Value> {
    let dir = std::env::temp_dir().join(format!("BluGHub-ghub-{}", std::process::id()));
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let result = read_copy(db, &dir);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn read_copy(db: &Path, dir: &Path) -> Result<Value> {
    let copy = dir.join("settings.db");
    std::fs::copy(db, &copy).with_context(|| format!("copying {}", db.display()))?;
    for ext in ["-wal", "-shm"] {
        let mut from = db.as_os_str().to_owned();
        from.push(ext);
        let from = PathBuf::from(from);
        if from.exists() {
            let mut to = copy.as_os_str().to_owned();
            to.push(ext);
            std::fs::copy(&from, PathBuf::from(to))
                .with_context(|| format!("copying {}", from.display()))?;
        }
    }
    // Opened read-write: it is a throwaway copy, and SQLite needs to write
    // the -shm file to read a WAL.
    let conn = rusqlite::Connection::open(&copy).context("opening the copy of G HUB's settings")?;
    let text: Vec<u8> = conn
        .query_row("SELECT file FROM data ORDER BY _id DESC LIMIT 1", [], |row| {
            Ok(match row.get_ref(0)? {
                rusqlite::types::ValueRef::Text(t) | rusqlite::types::ValueRef::Blob(t) => {
                    t.to_vec()
                }
                _ => Vec::new(),
            })
        })
        .context("reading G HUB's settings")?;
    serde_json::from_slice(&text).context("G HUB's settings are not the JSON expected")
}

/// The presets made for per-key keyboards, in G HUB's order.
pub fn presets(doc: &Value) -> Vec<Preset> {
    let Some(list) = doc.pointer("/lighting_prefabs/list").and_then(Value::as_array) else {
        return Vec::new();
    };
    list.iter()
        .filter(|p| {
            p.get("deviceSupport")
                .and_then(Value::as_array)
                .is_some_and(|s| s.iter().any(|d| d == "KEYBOARD_RGB_PER_KEY"))
        })
        .filter_map(|p| {
            let id = p.get("id")?.as_str()?.to_owned();
            let name = p.get("name").and_then(Value::as_str).unwrap_or(&id);
            // G HUB names its own ones e.g. "Freestyle effect (KEYBOARD_RGB_PER_KEY)".
            let name = name.trim_end_matches(" (KEYBOARD_RGB_PER_KEY)").to_owned();
            Some(Preset { id, name })
        })
        .collect()
}

/// The preset `id` as blu-ghub key colours.
pub fn layout(doc: &Value, id: &str) -> Result<Layout> {
    let groups = doc
        .get(format!("lighting_effects/{id}"))
        .ok_or_else(|| anyhow!("G HUB has no lighting effect {id}"))?
        .pointer("/fixedInfo/lightingSlots/infoMap")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("lighting effect {id} has no per-key colours"))?;
    let mut out = Layout::default();
    for (group, info) in groups {
        let Some(map) = info.pointer("/perKeyMap/colorCodeMap").and_then(Value::as_object) else {
            continue;
        };
        for (code, colour) in map {
            let key = code.parse().ok().and_then(|c| key_of(group, c));
            let rgb = colour.get("hex").and_then(Value::as_str).and_then(|h| h.parse().ok());
            match (key, rgb) {
                (Some(k), Some(c)) => {
                    out.keys.insert(k.name.to_owned(), c);
                }
                _ => out.skipped.push(format!("{group}:{code}")),
            }
        }
    }
    Ok(out)
}

/// The G915 key a G HUB key code stands for.
fn key_of(group: &str, code: u32) -> Option<Key> {
    let name = match (group, code) {
        ("PERKEY_KEYBOARD", u) => return keymap::by_usage(u),
        ("PERKEY_CONSUMER", 0xCD) => "play_pause",
        ("PERKEY_CONSUMER", 0xE2) => "mute",
        ("PERKEY_CONSUMER", 0xB5) => "next_track",
        ("PERKEY_CONSUMER", 0xB6) => "previous_track",
        ("PERKEY_GKEY", 1) => "g1",
        ("PERKEY_GKEY", 2) => "g2",
        ("PERKEY_GKEY", 3) => "g3",
        ("PERKEY_GKEY", 4) => "g4",
        ("PERKEY_GKEY", 5) => "g5",
        ("PERKEY_BRANDING", 1) => "logo",
        ("PERKEY_INDICATOR", 1) => "lighting",
        _ => return None,
    };
    keymap::by_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Invented colours and ids, in G HUB's shape.
    fn doc() -> Value {
        json!({
            "lighting_prefabs": {"list": [
                {"id": "11111111-0000-0000-0000-000000000001", "name": "Test effect (KEYBOARD_RGB_PER_KEY)",
                 "type": "FIXED", "deviceSupport": ["KEYBOARD_RGB_PER_KEY"]},
                {"id": "11111111-0000-0000-0000-000000000002", "name": "For a mouse",
                 "type": "FIXED", "deviceSupport": ["MOUSE"]},
            ]},
            "lighting_effects/11111111-0000-0000-0000-000000000001": {
                "deviceSupport": ["KEYBOARD_RGB_PER_KEY"],
                "fixedInfo": {"lightingSlots": {"infoMap": {
                    "PERKEY_KEYBOARD": {"perKeyMap": {"colorCodeMap": {
                        "41": {"hex": "#102030"},
                        "225": {"hex": "#405060"},
                        "49": {"hex": "#708090"},
                    }}},
                    "PERKEY_CONSUMER": {"perKeyMap": {"colorCodeMap": {"205": {"hex": "#a0b0c0"}}}},
                    "PERKEY_GKEY": {"perKeyMap": {"colorCodeMap": {"5": {"hex": "#010203"}}}},
                    "PERKEY_BRANDING": {"perKeyMap": {"colorCodeMap": {"1": {"hex": "#040506"}}}},
                    "PERKEY_INDICATOR": {"perKeyMap": {"colorCodeMap": {"1": {"hex": "nope"}}}},
                }}}
            }
        })
    }

    #[test]
    fn only_per_key_presets_are_listed_without_the_suffix() {
        assert_eq!(
            presets(&doc()),
            [Preset {
                id: "11111111-0000-0000-0000-000000000001".into(),
                name: "Test effect".into()
            }]
        );
    }

    #[test]
    fn a_preset_becomes_key_names() {
        let l = layout(&doc(), "11111111-0000-0000-0000-000000000001").unwrap();
        let get = |n: &str| l.keys.get(n).map(|c| c.to_string());
        assert_eq!(get("esc").as_deref(), Some("#102030"));
        assert_eq!(get("left_shift").as_deref(), Some("#405060"));
        assert_eq!(get("play_pause").as_deref(), Some("#a0b0c0"));
        assert_eq!(get("g5").as_deref(), Some("#010203"));
        assert_eq!(get("logo").as_deref(), Some("#040506"));
        assert_eq!(l.keys.len(), 5);
        let mut skipped = l.skipped.clone();
        skipped.sort();
        assert_eq!(skipped, ["PERKEY_INDICATOR:1", "PERKEY_KEYBOARD:49"]);
    }

    #[test]
    fn an_unknown_preset_is_an_error() {
        assert!(layout(&doc(), "nothing").is_err());
    }

    #[test]
    fn the_database_is_read_through_a_copy() {
        let dir = std::env::temp_dir().join(format!("blughub-ghub-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("settings.db");
        let _ = std::fs::remove_file(&db);
        {
            let c = rusqlite::Connection::open(&db).unwrap();
            c.execute_batch(
                "CREATE TABLE data(_id INTEGER PRIMARY KEY, _date_created datetime, file BLOB);",
            )
            .unwrap();
            c.execute("INSERT INTO data(_id, file) VALUES (1, ?1)", [doc().to_string().as_bytes()])
                .unwrap();
        }
        let before = std::fs::read(&db).unwrap();
        let read = read(&db).unwrap();
        assert_eq!(presets(&read).len(), 1);
        assert_eq!(std::fs::read(&db).unwrap(), before);
        let _ = std::fs::remove_dir_all(dir);
    }
}
