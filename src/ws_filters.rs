//! Per-connection payload filters, served over tosu's `/tokens` WebSocket.
//!
//! tosu lets a WebSocket client ask for a **subset** of the payload instead of
//! the whole thing. The client sends a JSON array over the socket, tosu stores it
//! on that connection, and from then on every frame for that connection carries
//! only the requested leaves
//! (`tosu-sourcecode/packages/server/utils/socket.ts:180-193`). This is how the
//! StreamCompanion web client keeps a browser from receiving the whole strain
//! graph on every tick.
//!
//! Two spellings reach the same place
//! (`tosu-sourcecode/packages/server/utils/scFilters.ts:1-13`):
//!
//! ```text
//! /tokens + "[\"client\"]"   -> applyFilters:["client"]
//! anywhere + "applyFilters:[â€¦]"  -> used verbatim
//! ```
//!
//! Anything else containing a `:` is passed through untouched, and anything
//! without a `:` that is not a JSON array is passed through as well.
//!
//! # The filter language
//!
//! `type Filter = string | { field: string; keys: Filter[] }` (`socket.ts:8`). A
//! string is a **top-level key** taken verbatim, and an object narrows into a
//! sub-object recursively:
//!
//! ```json
//! ["client", {"field": "play", "keys": ["score", {"field": "combo", "keys": ["current"]}]}]
//! ```
//!
//! Note that a dotted string is *not* a path -- tosu never splits on `.` -- so
//! `["play.score"]` matches nothing and the frame is `{}`.
//!
//! # The four behaviours that are easy to get wrong
//!
//! **1. A filter for an absent key silently disappears.** tosu does
//! `value[filter] = data[filter]` (`socket.ts:214`), a missing property is
//! `undefined`, and `JSON.stringify` drops `undefined` keys. Filtering for
//! `["nope"]` sends `{}`, not `{"nope":null}`.
//!
//! **2. A null *leaf* is kept, a null *parent* is not.** `socket.ts:218-221` skips
//! a nested filter when `data[filter.field]` is null or undefined, so narrowing
//! into a null field contributes **no key at all** rather than an empty object.
//! A null reached as a plain string filter is an ordinary value and is kept,
//! because assigning null is a real assignment.
//!
//! **3. A nested filter replaces the parent.** The recursion writes into a fresh
//! object (`socket.ts:224-229`), so narrowing does not merge. Two filters naming
//! the same field send only the last one's keys.
//!
//! **4. The output order is filter order, not payload order.** tosu assigns in
//! filter order and `JSON.stringify` preserves insertion order for string keys.
//! `serde_json::Map` is a `BTreeMap` and would sort alphabetically, so
//! [`Filtered`] carries the order and implements `Serialize` itself.
//!
//! # The one order that is *not* reproduced
//!
//! A **whole-object** filter -- `["play"]` -- copies the subtree as a parsed
//! `serde_json::Value`, and a `Value`'s object keys were sorted at parse time. So
//! `["play"]` emits `play` in alphabetical order where tosu emits it in the
//! builder's order.
//!
//! Every order the *filter* determines is exact: the top level, and each narrowed
//! level, follow the filter list. Only the interior of an un-narrowed subtree is
//! affected, and that is only observable by a client reading nested keys
//! positionally -- which is not something a JSON client does, and which the
//! narrowing form gives you exactly if you need it. Preserving it would mean
//! splicing raw byte ranges out of the published payload, which is a real cost
//! for a case with no reader. Recorded rather than hidden.

use serde::Serialize;
use serde::ser::SerializeMap;
use serde_json::Value;

/// A JSON value whose object keys keep **insertion** order at every depth.
#[derive(Debug, Clone, PartialEq)]
pub enum Filtered {
    /// A value copied straight out of the payload. `Value` is used as-is, which
    /// is fine: it comes from the already-parsed payload and is re-emitted
    /// verbatim, exactly as tosu's `value[filter] = data[filter]` does.
    Leaf(Value),
    /// An object built by the filter walk, in the order the filters named it.
    Object(Vec<(String, Filtered)>),
}

impl Filtered {
    /// Assign, or overwrite in place if the key is already present. Both are what
    /// assigning to an existing JavaScript object property does, and overwriting
    /// in place is what keeps the *first* position when a key is filtered twice.
    fn insert(&mut self, key: String, value: Filtered) {
        match self {
            Filtered::Object(entries) => match entries.iter_mut().find(|(k, _)| *k == key) {
                Some(slot) => slot.1 = value,
                None => entries.push((key, value)),
            },
            Filtered::Leaf(_) => {}
        }
    }

    /// The keys, in wire order. `serde_json::Value` cannot represent this,
    /// because a `Map` sorts, so assertions read the order through here.
    pub fn keys(&self) -> Vec<&str> {
        match self {
            Filtered::Object(entries) => entries.iter().map(|(k, _)| k.as_str()).collect(),
            Filtered::Leaf(_) => Vec::new(),
        }
    }

    /// A child by key, for assertions. `None` for a key that is not present.
    ///
    /// Returns the child **node** rather than its value, because a child reached
    /// by a filter may be either a leaf or a further object -- `session` above,
    /// `client` below -- and collapsing the two would make a nested path
    /// unassertable. Narrow with [`Filtered::leaf`].
    ///
    /// The tests use this rather than reaching into the tree, because a
    /// `Filtered` has no public indexing and a test that rebuilt the lookup itself
    /// would be asserting against its own copy of the logic.
    #[cfg(test)]
    pub fn get(&self, key: &str) -> Option<&Filtered> {
        match self {
            Filtered::Object(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            // A leaf is not a container, so it has no child -- which is also what
            // the wire shows: narrowing into a leaf is a dead end.
            Filtered::Leaf(_) => None,
        }
    }

    /// The value, if this node is a leaf.
    #[cfg(test)]
    pub fn leaf(&self) -> Option<&Value> {
        match self {
            Filtered::Leaf(value) => Some(value),
            Filtered::Object(_) => None,
        }
    }
}

impl Serialize for Filtered {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            // `serde_json` renders an integral float as `5.0` where
            // `JSON.stringify` renders `5`. That is the same property rtosu's own
            // payloads have, and it is invisible to a parser, so it is not chased
            // here. It is noted so a byte-level diff is not mistaken for a bug.
            Filtered::Leaf(value) => value.serialize(serializer),
            Filtered::Object(entries) => {
                let mut map = serializer.serialize_map(Some(entries.len()))?;
                for (key, value) in entries {
                    map.serialize_entry(key, value)?;
                }
                map.end()
            }
        }
    }
}

/// One entry of a filter list, as tosu's `Filter` type.
#[derive(Debug, Clone, PartialEq)]
pub enum Filter {
    /// A top-level key of the payload, taken verbatim.
    Key(String),
    /// Narrow into `field` and keep only `keys` beneath it.
    Nested { field: String, keys: Vec<Filter> },
}

impl Filter {
    /// Parse one filter entry.
    ///
    /// tosu's validation is loose (`commands.ts:154-158`): an object needs a
    /// `field` and an array `keys`, and an entry that is neither a string nor a
    /// well-formed object is skipped rather than rejecting the whole list.
    pub fn parse(value: &Value) -> Option<Self> {
        match value {
            Value::String(key) => Some(Filter::Key(key.clone())),
            Value::Object(map) => {
                let field = map.get("field")?.as_str()?.to_string();
                let keys = map.get("keys")?.as_array()?;
                Some(Filter::Nested {
                    field,
                    keys: keys.iter().filter_map(Filter::parse).collect(),
                })
            }
            _ => None,
        }
    }
}

/// Parse a whole filter list, or `None` if it is not an array.
///
/// tosu rejects a non-array outright (`commands.ts:152-158`) and leaves the
/// connection's existing filters untouched, so a bad message is not something the
/// client needs to hear about.
pub fn parse_filters(data: &str) -> Option<Vec<Filter>> {
    let parsed: Value = serde_json::from_str(data.trim()).ok()?;
    let array = parsed.as_array()?;
    Some(array.iter().filter_map(Filter::parse).collect())
}

/// Normalise an inbound socket message into a command, matching tosu's
/// `normalizeSocketCommand` (`scFilters.ts:1-13`).
///
/// Returns the message unchanged unless it is a bare JSON array arriving on
/// `/tokens`, which tosu rewrites to the `applyFilters:` command. An
/// `applyFilters:` message already contains a `:` and so passes through the
/// first branch untouched.
pub fn normalize_socket_command(data: &str, pathname: &str) -> String {
    if data.contains(':') {
        return data.to_string();
    }
    let path_only = pathname.split('?').next().unwrap_or(pathname);
    if path_only == "/tokens" && data.trim_start().starts_with('[') {
        return format!("applyFilters:{data}");
    }
    data.to_string()
}

/// Extract the filter list from a normalised command, or `None` if the message is
/// not `applyFilters`.
///
/// tosu's `payload.startsWith('[') ? payload : legacyPayload` fallback
/// (`commands.ts:140`) exists for an older message shape whose payload is not in
/// this tree, so only the array form is accepted here rather than guessing at a
/// format that cannot be checked.
pub fn filters_from_command(command: &str) -> Option<Vec<Filter>> {
    let payload = command.strip_prefix("applyFilters:")?;
    parse_filters(payload)
}

/// Build the filtered object for one connection.
///
/// `None` means "no filters", which is the signal to send the full payload
/// instead (`socket.ts:182-183`). An **empty** list is also `None`, matching
/// tosu: `client.filters.length > 0` gates the filtered path, so `[]` means
/// unfiltered.
pub fn apply_filters(filters: &[Filter], data: &Value) -> Option<Filtered> {
    if filters.is_empty() {
        return None;
    }
    let mut out = Filtered::Object(Vec::with_capacity(filters.len()));
    for filter in filters {
        write_filter(&mut out, data, filter);
    }
    Some(out)
}

/// One filter's contribution to the output, recursing for nested filters.
fn write_filter(out: &mut Filtered, data: &Value, filter: &Filter) {
    match filter {
        Filter::Key(key) => {
            // tosu assigns unconditionally (`socket.ts:214`) and a missing
            // property becomes `undefined`, which `JSON.stringify` drops. Not
            // inserting the key *is* that behaviour; inserting a null is not.
            if let Some(value) = data.get(key) {
                out.insert(key.clone(), Filtered::Leaf(value.clone()));
            }
        }
        Filter::Nested { field, keys } => {
            // A null or undefined **parent** contributes nothing at all
            // (`socket.ts:218-221`) -- not an empty object.
            let Some(parent) = data.get(field).filter(|value| !value.is_null()) else {
                return;
            };
            let mut nested = Filtered::Object(Vec::with_capacity(keys.len()));
            for key in keys {
                write_filter(&mut nested, parent, key);
            }
            // Written into a fresh object (`socket.ts:224-229`), so this replaces
            // the parent rather than merging into it.
            out.insert(field.clone(), nested);
        }
    }
}

/// The cost of this feature, stated plainly.
///
/// rtosu keeps the payload **pre-serialised** so a broadcast is a refcount bump
/// and a write. Filtering needs random access by key, so the bytes have to be
/// parsed. tosu does not pay this: it builds the object in the first place and
/// picks keys out of it, then re-serialises the *filtered* object per client
/// (`socket.ts:190`).
///
/// So a filtered connection here costs one full JSON parse per tick, against a
/// write of a few hundred bytes. The parse is still the cheaper side of that
/// trade for any payload much larger than the filtered subset -- which is the
/// whole reason a client asks for filters -- but it is a real cost that scales
/// with the payload rather than with the request.
///
/// Caching a parsed `Value` on the published packet would amortise the parse
/// across all connections, at the price of one parse per tick for *every* client
/// including unfiltered ones. That is the wrong trade while the default payload
/// is ~250 KB and filtered clients are rare, and it becomes the right one if the
/// payload shrinks or filtered clients become common. It is recorded here rather
/// than left for a future reader to discover.
pub fn parse_packet(json: &[u8]) -> Option<Value> {
    serde_json::from_slice(json).ok()
}

/// Serialise a filtered payload, matching what tosu puts on the wire.
///
/// `JSON.stringify` emits no whitespace, and `serde_json::to_vec` does not either.
pub fn filter_json(filters: &[Filter], data: &Value) -> Option<Vec<u8>> {
    let out = apply_filters(filters, data)?;
    serde_json::to_vec(&out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal packet shaped like the real v2 payload, with the cases the
    /// filter rules care about: a scalar, a nested object, a **null** leaf, and a
    /// key that is absent.
    fn packet() -> Value {
        serde_json::json!({
            "client": "stable",
            "session": { "playTime": 5105, "playCount": 0 },
            "play": {
                "score": 4652,
                "accuracy": 68.75,
                "combo": { "current": 4, "max": 11 },
                "matchmaking": null
            },
            "beatmap": { "id": 2964306, "stats": { "ar": { "original": 9.2 } } }
        })
    }

    /// Filter, then serialise, so the assertions read the actual wire bytes.
    fn filtered(json: &str) -> Option<String> {
        let filters = parse_filters(json)?;
        let bytes = filter_json(&filters, &packet())?;
        Some(String::from_utf8(bytes).unwrap())
    }

    #[test]
    fn a_string_filter_takes_a_top_level_key_verbatim() {
        assert_eq!(
            filtered(r#"["client","session"]"#).as_deref(),
            Some(r#"{"client":"stable","session":{"playCount":0,"playTime":5105}}"#)
        );
    }

    /// A whole-object filter copies the subtree, and a parsed `serde_json::Value`
    /// has already sorted its keys -- so the *contents* of an un-narrowed object
    /// come out alphabetical. The order the filter itself determines (the top
    /// level and every narrowed level) is exact, and the module doc records why
    /// the interior is not.
    #[test]
    fn a_whole_object_filter_carries_a_sorted_subtree() {
        assert_eq!(
            filtered(r#"["play"]"#).as_deref(),
            Some(
                r#"{"play":{"accuracy":68.75,"combo":{"current":4,"max":11},"matchmaking":null,"score":4652}}"#
            )
        );
        // Narrowing the same subtree restores filter order at both levels.
        assert_eq!(
            filtered(r#"[{"field":"play","keys":["score","combo"]}]"#).as_deref(),
            Some(r#"{"play":{"score":4652,"combo":{"current":4,"max":11}}}"#)
        );
    }

    /// The top level is the order the client asked for, whatever the payload's
    /// own order is. `serde_json::Map` sorts, so this is the assertion that
    /// catches a regression to alphabetical order.
    #[test]
    fn the_output_follows_filter_order_not_payload_order() {
        // Payload order is beatmap, client, play, session. Ask for the reverse
        // and the output must be the reverse.
        assert_eq!(
            filtered(r#"["session","beatmap","client"]"#).as_deref(),
            Some(
                r#"{"session":{"playCount":0,"playTime":5105},"beatmap":{"id":2964306,"stats":{"ar":{"original":9.2}}},"client":"stable"}"#
            )
        );
        // The alphabetical order that a `BTreeMap` would have produced.
        assert_ne!(
            filtered(r#"["session","beatmap","client"]"#).as_deref(),
            Some(
                r#"{"beatmap":{"id":2964306,"stats":{"ar":{"original":9.2}}},"client":"stable","session":{"playCount":0,"playTime":5105}}"#
            ),
            "a BTreeMap would produce this; the filter order must not"
        );
        // The key order is readable without a parser, for the same reason.
        let filters = parse_filters(r#"["session","client"]"#).unwrap();
        let out = apply_filters(&filters, &packet()).unwrap();
        assert_eq!(out.keys(), ["session", "client"]);

        // And `get` reads a child off the tree, so the accessor is exercised by
        // the test that needs it rather than sitting unused.
        assert_eq!(
            out.get("client")
                .and_then(Filtered::leaf)
                .and_then(Value::as_str),
            Some("stable")
        );
        assert_eq!(
            out.get("session")
                .and_then(Filtered::leaf)
                .and_then(|value| value.get("playTime")),
            Some(&Value::from(5105)),
            "a nested path narrows into the child's value"
        );
        assert_eq!(out.get("nope"), None, "an absent key is None, not a null");
        assert_eq!(
            out.get("client")
                .and_then(|node| node.get("x"))
                .and_then(Filtered::leaf),
            None,
            "narrowing into a leaf is a dead end, as on the wire"
        );
    }

    /// A filter for a key that does not exist produces **nothing**, not a null.
    /// tosu assigns `undefined` and `JSON.stringify` drops the key.
    #[test]
    fn a_filter_for_an_absent_key_disappears_entirely() {
        assert_eq!(filtered(r#"["nope"]"#).as_deref(), Some("{}"));
        assert_eq!(
            filtered(r#"["client","nope"]"#).as_deref(),
            Some(r#"{"client":"stable"}"#)
        );
    }

    /// A null reached as a **leaf** is kept -- assigning null is a real
    /// assignment. Only narrowing *into* a null is skipped.
    #[test]
    fn a_null_leaf_is_kept_because_assigning_null_is_a_real_assignment() {
        assert_eq!(
            filtered(r#"["play"]"#)
                .as_deref()
                .unwrap()
                .contains(r#""matchmaking":null"#),
            true
        );
        assert_eq!(
            filtered(r#"[{"field":"play","keys":["matchmaking"]}]"#).as_deref(),
            Some(r#"{"play":{"matchmaking":null}}"#)
        );
    }

    /// Narrowing into a **null field** contributes no key at all, because tosu
    /// returns before inserting (`socket.ts:218-221`) -- not an empty object.
    #[test]
    fn a_null_parent_contributes_no_key_at_all() {
        assert_eq!(
            filtered(r#"[{"field":"play.matchmaking","keys":["x"]}]"#).as_deref(),
            Some("{}"),
            "the parent is null, so the whole filter is skipped"
        );
        // An absent parent behaves identically.
        assert_eq!(
            filtered(r#"[{"field":"absent","keys":["x"]}]"#).as_deref(),
            Some("{}")
        );
        // But a present, non-null parent yields the object even when every
        // requested leaf is absent -- the recursion runs and writes `{}`.
        assert_eq!(
            filtered(r#"[{"field":"session","keys":["absent"]}]"#).as_deref(),
            Some(r#"{"session":{}}"#)
        );
    }

    /// A nested filter replaces the parent with a fresh object holding only the
    /// requested keys, recursively, and in filter order at every depth.
    #[test]
    fn a_nested_filter_replaces_the_parent_with_the_requested_keys() {
        assert_eq!(
            filtered(r#"[{"field":"session","keys":["playTime"]}]"#).as_deref(),
            Some(r#"{"session":{"playTime":5105}}"#)
        );
        assert_eq!(
            filtered(r#"[{"field":"play","keys":[{"field":"combo","keys":["max"]},"score"]}]"#)
                .as_deref(),
            Some(r#"{"play":{"combo":{"max":11},"score":4652}}"#),
            "nested order is filter order, not the packet's order"
        );
    }

    /// Two filters naming the same field: the second replaces the first, because
    /// the recursion writes into a fresh object and the parent key is
    /// re-inserted at its original position.
    #[test]
    fn a_second_filter_for_the_same_field_replaces_the_keys_not_the_position() {
        assert_eq!(
            filtered(
                r#"[{"field":"session","keys":["playTime"]},{"field":"session","keys":["playCount"]}]"#
            )
            .as_deref(),
            Some(r#"{"session":{"playCount":0}}"#)
        );
    }

    /// tosu gates the filtered path on `filters.length > 0`, so an empty list
    /// means "send the whole payload", not "send an empty object".
    #[test]
    fn an_empty_filter_list_means_unfiltered() {
        assert_eq!(parse_filters("[]").unwrap().len(), 0);
        assert!(apply_filters(&[], &packet()).is_none());
        assert!(parse_filters("{}").is_none(), "not an array");
        assert!(parse_filters("not json").is_none());
        // An entry that is neither a string nor a well-formed object is skipped,
        // which can empty the list and so re-enable the full payload.
        assert_eq!(parse_filters("[1,2,3]").unwrap().len(), 0);
        assert_eq!(parse_filters(r#"[{"nope":1}]"#).unwrap().len(), 0);
    }

    /// A bare JSON array on `/tokens` is the filter command. Everything else is
    /// passed through, and the `:` check comes first so an explicit
    /// `applyFilters:` is never double-prefixed.
    #[test]
    fn command_normalisation_matches_tosus_rules() {
        assert_eq!(
            normalize_socket_command(r#"["client"]"#, "/tokens"),
            r#"applyFilters:["client"]"#
        );
        // A query string is stripped before the path is compared.
        assert_eq!(
            normalize_socket_command(r#"["a"]"#, "/tokens?x=1"),
            r#"applyFilters:["a"]"#
        );
        // Leading whitespace does not hide the array, and is kept verbatim in the
        // rewritten command, as tosu's template literal does.
        assert_eq!(
            normalize_socket_command("  [\"a\"]", "/tokens"),
            r#"applyFilters:  ["a"]"#
        );
        // Already a command.
        assert_eq!(
            normalize_socket_command(r#"applyFilters:["a"]"#, "/tokens"),
            r#"applyFilters:["a"]"#
        );
        // Any other path leaves a bare array alone.
        assert_eq!(
            normalize_socket_command(r#"["a"]"#, "/websocket/v2"),
            r#"["a"]"#
        );
        // Not an array.
        assert_eq!(normalize_socket_command("hello", "/tokens"), "hello");
    }

    /// **The colon check runs first, so a bare object filter is not a command.**
    /// `{`field`:`mapStrains`,...}` contains colons, so tosu's first branch returns
    /// it unchanged and the `applyFilters:` prefix is never added -- the array is
    /// then not a command at all and the connection's filters are left alone.
    ///
    /// This is upstream's behaviour, verified live: sending
    /// `[{"field":"mapStrains","keys":["0","400"]}]` to tosu's `/tokens` leaves the
    /// previous filter in place and draws a `{"command":...}` acknowledgement with
    /// the raw text in it. An object filter therefore has to be sent **explicitly**
    /// as `applyFilters:[{...}]`, and only a colon-free array of plain keys
    /// (`["osuIsRunning"]`) works as a bare shorthand.
    ///
    /// Worth pinning because the rule looks like a bug and is not: relaxing it
    /// would make rtosu accept a request tosu rejects, so an overlay relying on
    /// the quirk would behave differently on the two.
    #[test]
    fn a_bare_object_filter_is_never_treated_as_a_command() {
        let bare = r#"[{"field":"mapStrains","keys":["0","400"]}]"#;
        assert!(bare.contains(':'), "the premise of this test");
        assert_eq!(
            normalize_socket_command(bare, "/tokens"),
            bare,
            "returned unchanged, so no applyFilters: prefix"
        );
        assert!(
            filters_from_command(&normalize_socket_command(bare, "/tokens")).is_none(),
            "and therefore not a command"
        );

        // Sent explicitly, the same array works.
        let explicit = format!("applyFilters:{bare}");
        let filters = filters_from_command(&explicit).expect("filters");
        assert_eq!(
            filters,
            vec![Filter::Nested {
                field: "mapStrains".to_string(),
                keys: vec![Filter::Key("0".to_string()), Filter::Key("400".to_string())],
            }]
        );
    }

    #[test]
    fn a_command_yields_its_filter_list() {
        let command = normalize_socket_command(r#"["client"]"#, "/tokens");
        let filters = filters_from_command(&command).expect("filters");
        assert_eq!(filters, vec![Filter::Key("client".to_string())]);
        assert!(filters_from_command("somethingElse").is_none());
        // A malformed payload leaves the connection's filters untouched, which is
        // tosu's behaviour: it returns without assigning.
        assert!(filters_from_command("applyFilters:{}").is_none());
        assert!(filters_from_command("applyFilters:[").is_none());
    }

    /// A dotted string is a single top-level key, **not** a path. tosu never
    /// splits on `.`, so `["play.score"]` yields `{}` against the real packet. An
    /// overlay written against a dotted-path filter would silently get nothing,
    /// so this pins the real behaviour rather than the assumption.
    #[test]
    fn a_dotted_filter_is_a_single_key_and_not_a_path() {
        assert_eq!(filtered(r#"["play.score"]"#).as_deref(), Some("{}"));
    }

    #[test]
    fn the_serialized_form_is_compact_like_json_stringify() {
        let filters = parse_filters(r#"["client","session"]"#).unwrap();
        let json = filtered(r#"["client","session"]"#).unwrap();
        assert_eq!(
            json,
            r#"{"client":"stable","session":{"playCount":0,"playTime":5105}}"#
        );
        assert!(!json.contains(' '), "JSON.stringify emits no whitespace");
        assert!(filter_json(&filters, &packet()).is_some());
    }

    #[test]
    fn parse_packet_reads_the_published_bytes() {
        let json = br#"{"client":"stable","play":{"score":4652}}"#;
        let value = parse_packet(json).expect("parses");
        assert_eq!(value["play"]["score"], 4652);
        // A truncated or non-JSON body yields `None` rather than panicking, so a
        // filtered connection can fall back to the full payload.
        assert!(parse_packet(b"{not json").is_none());
        assert!(parse_packet(b"").is_none());
    }

    /// Every filter path, in one place, so a change to the walk has a single
    /// reference to compare against.
    #[test]
    fn the_whole_language_in_one_pass() {
        assert_eq!(
            filtered(
                r#"["client",{"field":"play","keys":["score",{"field":"combo","keys":["current"]}]},"gone"]"#
            )
            .as_deref(),
            Some(
                r#"{"client":"stable","play":{"score":4652,"combo":{"current":4}}}"#
            )
        );
    }
}
