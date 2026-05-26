//! Locale-catalog structural-parity gate.
//!
//! Asserts that every non-`en` locale has the same flat dotted key set
//! as `en`, and that each value's `%{var}` placeholder *set* matches
//! between locales. Does NOT verify translation quality.

#![allow(clippy::collapsible_if)]

use std::collections::BTreeSet;

const EN: &str = include_str!("../locales/en.toml");
const ES: &str = include_str!("../locales/es.toml");
const PT: &str = include_str!("../locales/pt.toml");
const HI: &str = include_str!("../locales/hi.toml");

#[test]
fn every_locale_has_the_same_key_set_as_en() {
    let en_keys = flatten_keys(EN);
    for (label, raw) in [("es", ES), ("pt", PT), ("hi", HI)] {
        let other = flatten_keys(raw);
        let only_in_en: BTreeSet<_> = en_keys.difference(&other).collect();
        let only_in_other: BTreeSet<_> = other.difference(&en_keys).collect();
        assert!(
            only_in_en.is_empty() && only_in_other.is_empty(),
            "locale `{label}` key set diverges from `en`:\n  only in en: {only_in_en:?}\n  only in {label}: {only_in_other:?}"
        );
    }
}

#[test]
fn every_locale_has_the_same_placeholder_set_per_key() {
    let en = flatten(EN);
    for (label, raw) in [("es", ES), ("pt", PT), ("hi", HI)] {
        let other = flatten(raw);
        for (k, en_value) in &en {
            if let Some(other_value) = other.get(k) {
                let en_p = placeholders(en_value);
                let other_p = placeholders(other_value);
                assert_eq!(
                    en_p, other_p,
                    "locale `{label}` key `{k}` placeholder set differs from `en`: en={en_p:?} {label}={other_p:?}"
                );
            }
        }
    }
}

fn flatten(raw: &str) -> std::collections::BTreeMap<String, String> {
    let value: toml::Value = toml::from_str(raw).expect("locale TOML must parse");
    let mut out = std::collections::BTreeMap::new();
    flatten_into(&value, String::new(), &mut out);
    out
}

fn flatten_keys(raw: &str) -> BTreeSet<String> {
    flatten(raw).into_keys().collect()
}

fn flatten_into(
    v: &toml::Value,
    prefix: String,
    out: &mut std::collections::BTreeMap<String, String>,
) {
    match v {
        toml::Value::Table(t) => {
            for (k, child) in t {
                let next = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten_into(child, next, out);
            }
        }
        toml::Value::String(s) => {
            out.insert(prefix, s.clone());
        }
        other => panic!("unexpected non-string value at `{prefix}`: {other:?}"),
    }
}

fn placeholders(value: &str) -> BTreeSet<String> {
    // Extract every `%{name}` occurrence.
    let mut out = BTreeSet::new();
    let bytes = value.as_bytes();
    let mut i = 0;
    while i + 2 < bytes.len() {
        if bytes[i] == b'%' && bytes[i + 1] == b'{' {
            if let Some(end_rel) = value[i + 2..].find('}') {
                let name = &value[i + 2..i + 2 + end_rel];
                out.insert(name.to_string());
                i += 2 + end_rel + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}
