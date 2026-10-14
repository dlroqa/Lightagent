//! Runs Web's candidate `LA-WEB-PROVISIONING/1` fixtures, byte for byte as
//! mirrored under `docs/integrations/web/la-web-provisioning-1-r1/`, through
//! this crate's independent verifier.
//!
//! The mirror is pinned by the compatibility digest below. Any change to the
//! specification or a fixture changes that digest and fails the first test,
//! so a new candidate has to be reviewed and pinned deliberately.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use lightagent_protocol_conformance::{
    SIGNING_CONTEXT, TrustedKey, Verifier, base64url, hex_lower, jcs, parse_strict, payload_hash,
};
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::Value as Json;
use sha2::{Digest, Sha256};

/// SHA-256 of the exact bytes of `MANIFEST.json` for candidate
/// `la-web-provisioning-1/r1`, as reported by Web.
const COMPATIBILITY_DIGEST: &str =
    "030537b8be8cc003cff31e503343d5082629eab3f3f10c82fb17d7776b24a1d2";
const FIXTURES: &str = "protocol/la-web-provisioning-1";

fn mirror() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/integrations/web/la-web-provisioning-1-r1")
}

fn bytes(path: &str) -> Vec<u8> {
    let full = mirror().join(path);
    std::fs::read(&full).unwrap_or_else(|e| panic!("read {}: {e}", full.display()))
}

fn json(file: &str) -> Json {
    serde_json::from_slice(&bytes(&format!("{FIXTURES}/{file}"))).unwrap()
}

fn sha256_hex(data: &[u8]) -> String {
    hex_lower(&Sha256::digest(data))
}

fn s(value: &Json) -> &str {
    value.as_str().unwrap()
}

#[test]
fn the_mirror_is_the_pinned_candidate() {
    let manifest_bytes = bytes(&format!("{FIXTURES}/MANIFEST.json"));
    assert_eq!(
        sha256_hex(&manifest_bytes),
        COMPATIBILITY_DIGEST,
        "MANIFEST.json is not the reviewed candidate"
    );
    let manifest: Json = serde_json::from_slice(&manifest_bytes).unwrap();
    assert_eq!(manifest["protocol"], "LA-WEB-PROVISIONING/1");
    assert_eq!(manifest["fixtureSet"], "la-web-provisioning-1/r1");

    let spec = &manifest["specification"];
    let spec_bytes = bytes(s(&spec["path"]));
    assert_eq!(spec_bytes.len() as u64, spec["bytes"].as_u64().unwrap());
    assert_eq!(sha256_hex(&spec_bytes), s(&spec["sha256"]));

    // Every fixture file: size and hash, then fixtureDigest (§15).
    let mut lines = Vec::new();
    for file in manifest["files"].as_array().unwrap() {
        let path = s(&file["path"]);
        let data = bytes(path);
        assert_eq!(data.len() as u64, file["bytes"].as_u64().unwrap(), "{path}");
        let hash = sha256_hex(&data);
        assert_eq!(hash, s(&file["sha256"]), "{path}");
        lines.push((path.to_owned(), hash));
    }
    lines.sort();
    let listing: String = lines.iter().map(|(p, h)| format!("{h}  {p}\n")).collect();
    assert_eq!(
        sha256_hex(listing.as_bytes()),
        s(&manifest["fixtureDigest"])
    );

    // MANIFEST.sha256 lists the same hashes (sha256sum -c format).
    let listed = String::from_utf8(bytes(&format!("{FIXTURES}/MANIFEST.sha256"))).unwrap();
    let listed: BTreeMap<&str, &str> = listed
        .lines()
        .map(|l| {
            let (hash, path) = l.split_once("  ").unwrap();
            (path, hash)
        })
        .collect();
    assert_eq!(listed[s(&spec["path"])], s(&spec["sha256"]));
    assert_eq!(
        listed[&*format!("{FIXTURES}/MANIFEST.json")],
        COMPATIBILITY_DIGEST
    );
    for (path, hash) in &lines {
        assert_eq!(listed[path.as_str()], hash, "{path}");
    }

    // The manifest names every vector id, and only those.
    for (path, ids) in manifest["fixtureIds"].as_object().unwrap() {
        let file: Json = serde_json::from_slice(&bytes(path)).unwrap();
        let list = file
            .get("vectors")
            .or_else(|| file.get("cases"))
            .and_then(Json::as_array)
            .map(|v| v.iter().map(|x| s(&x["id"]).to_owned()).collect::<Vec<_>>())
            .unwrap_or_default();
        let named: Vec<String> = ids
            .as_array()
            .unwrap()
            .iter()
            .map(|x| s(x).to_owned())
            .collect();
        assert_eq!(list, named, "{path}");
    }
}

struct Keys {
    /// name → (key id, seed, public key)
    by_name: BTreeMap<String, (String, [u8; 32], [u8; 32])>,
    states: Json,
}

fn keys() -> Keys {
    let doc = json("keys.json");
    let mut by_name = BTreeMap::new();
    for key in doc["keys"].as_array().unwrap() {
        let seed: [u8; 32] = hex::decode(s(&key["seedHex"])).unwrap().try_into().unwrap();
        let public: [u8; 32] = hex::decode(s(&key["publicKeyHex"]))
            .unwrap()
            .try_into()
            .unwrap();
        by_name.insert(
            s(&key["name"]).to_owned(),
            (s(&key["keyId"]).to_owned(), seed, public),
        );
    }
    Keys {
        by_name,
        states: doc["verifierStates"].clone(),
    }
}

impl Keys {
    fn verifier(&self, state: &str, now: u64, seen: &Json) -> Verifier {
        let state = &self.states[state];
        Verifier {
            harness_id: s(&state["harnessId"]).to_owned(),
            keys: state["trustedKeys"]
                .as_array()
                .unwrap()
                .iter()
                .map(|k| TrustedKey {
                    key_id: s(&k["keyId"]).to_owned(),
                    public_key: base64url(s(&k["publicKey"]), 32)
                        .expect("config key")
                        .try_into()
                        .unwrap(),
                    revoked: match s(&k["status"]) {
                        "active" => false,
                        "revoked" => true,
                        other => panic!("key status {other}"),
                    },
                })
                .collect(),
            now,
            seen_nonces: seen
                .as_array()
                .unwrap()
                .iter()
                .map(|n| s(n).to_owned())
                .collect(),
        }
    }
}

#[test]
fn test_keys_derive_from_their_public_labels() {
    let doc = json("keys.json");
    for key in doc["keys"].as_array().unwrap() {
        let seed = Sha256::digest(s(&key["label"]).as_bytes());
        assert_eq!(hex_lower(&seed), s(&key["seedHex"]));
        let pair = Ed25519KeyPair::from_seed_unchecked(&seed).unwrap();
        assert_eq!(
            hex_lower(pair.public_key().as_ref()),
            s(&key["publicKeyHex"])
        );
        assert_eq!(
            base64url(s(&key["publicKey"]), 32).unwrap(),
            pair.public_key().as_ref()
        );
    }
    assert_eq!(
        hex_lower(SIGNING_CONTEXT),
        s(&doc["constants"]["signingContextHex"])
    );
}

fn registry() -> BTreeMap<String, u64> {
    json("errors.json")["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (s(&e["code"]).to_owned(), e["http"].as_u64().unwrap()))
        .collect()
}

/// Every `commands-*` and `redelivery` vector gives exactly its `expect`
/// (§15): accepted, or refused with the first failing step's code and that
/// code's registered HTTP status. All mismatches are reported together.
#[test]
fn every_command_vector_gives_its_expected_result() {
    let keys = keys();
    let registry = registry();
    let mut failures = Vec::new();
    let mut counts = BTreeMap::<&str, (usize, usize)>::new();
    for file in [
        "commands-valid.json",
        "commands-invalid.json",
        "redelivery.json",
    ] {
        for vector in json(file)["vectors"].as_array().unwrap() {
            let id = s(&vector["id"]);
            let body = hex::decode(s(&vector["bodyHex"])).unwrap();
            let mut verifier = keys.verifier(
                s(&vector["verifier"]),
                vector["now"].as_u64().unwrap(),
                &vector["seenNonces"],
            );
            let got = verifier.verify(&body).map(|_| ()).err();
            let expect = &vector["expect"];
            let want = if expect["ok"].as_bool().unwrap() {
                None
            } else {
                let code = s(&expect["code"]);
                assert_eq!(
                    registry.get(code).copied(),
                    expect["http"].as_u64(),
                    "{id}: HTTP status disagrees with errors.json"
                );
                Some(code)
            };
            let entry = counts.entry(file).or_default();
            entry.0 += 1;
            if got == want {
                entry.1 += 1;
            } else {
                failures.push(format!("{file} {id}: expected {want:?}, got {got:?}"));
            }
        }
    }
    for (file, (total, passed)) in &counts {
        println!("{file}: {passed}/{total} vectors as expected");
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert_eq!(counts["commands-valid.json"].0, 15);
    assert_eq!(counts["commands-invalid.json"].0, 132);
    assert_eq!(counts["redelivery.json"].0, 6);
}

/// For each signed transmission with details: Rust reproduces the canonical
/// JSON, the signing input, the payload hash, the deterministic Ed25519
/// signature from the test seed, and the exact wire body.
#[test]
fn canonical_bytes_and_signatures_are_reproduced_in_rust() {
    let keys = keys();
    let mut checked = 0;
    for file in ["commands-valid.json", "redelivery.json"] {
        for vector in json(file)["vectors"].as_array().unwrap() {
            let detail = &vector["detail"];
            if detail.get("command").is_none() {
                continue;
            }
            let id = s(&vector["id"]);
            let body = hex::decode(s(&vector["bodyHex"])).unwrap();
            assert_eq!(body, s(&vector["bodyText"]).as_bytes(), "{id}: bodyText");
            let mut verifier = keys.verifier(
                s(&vector["verifier"]),
                vector["now"].as_u64().unwrap(),
                &vector["seenNonces"],
            );
            let verified = verifier
                .verify(&body)
                .unwrap_or_else(|c| panic!("{id}: {c}"));
            assert_eq!(
                verified.unsigned_canonical,
                s(&detail["canonicalJson"]).as_bytes(),
                "{id}: canonical JSON"
            );
            let mut input = SIGNING_CONTEXT.to_vec();
            input.extend_from_slice(&verified.unsigned_canonical);
            assert_eq!(hex_lower(&input), s(&detail["signingInputHex"]), "{id}");
            assert_eq!(verified.payload_hash, s(&detail["payloadHash"]), "{id}");

            // The fixture's `command` object (fixture-file JSON) canonicalizes
            // to the same bytes through the strict path.
            let fixture_command = parse_strict(
                serde_json::to_string(&detail["command"])
                    .unwrap()
                    .as_bytes(),
            )
            .unwrap();
            assert_eq!(jcs(&fixture_command), verified.unsigned_canonical, "{id}");

            let (_, seed, public) = &keys.by_name[s(&detail["signedBy"])];
            let pair = Ed25519KeyPair::from_seed_and_public_key(seed, public).unwrap();
            let signature = pair.sign(&input);
            let encoded = base64url_encode(signature.as_ref());
            assert_eq!(encoded, s(&detail["signature"]), "{id}: signature");

            // The wire body is JCS of the command including the signature.
            assert_eq!(jcs(&verified.command), body, "{id}: wire body");
            checked += 1;
        }
    }
    assert_eq!(checked, 20, "15 valid commands and 5 detailed redeliveries");
}

fn base64url_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    let (mut acc, mut bits) = (0u32, 0u32);
    for &b in data {
        acc = (acc << 8) | u32::from(b);
        bits += 8;
        while bits >= 6 {
            bits -= 6;
            out.push(ALPHABET[((acc >> bits) & 63) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((acc << (6 - bits)) & 63) as usize] as char);
    }
    out
}

/// §9.1: transmissions of one logical command repeat its identity and
/// byte-identical payload, and renew the per-transmission members.
#[test]
fn redeliveries_keep_one_logical_command() {
    let doc = json("redelivery.json");
    let vectors: BTreeMap<&str, &Json> = doc["vectors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| (s(&v["id"]), v))
        .collect();
    let command = |id: &str| {
        let body = hex::decode(s(&vectors[id]["bodyHex"])).unwrap();
        parse_strict(&body).unwrap()
    };
    for logical in doc["logicalCommands"].as_array().unwrap() {
        let ids: Vec<&str> = logical["transmissions"]
            .as_array()
            .unwrap()
            .iter()
            .map(s)
            .collect();
        assert!(ids.len() >= 2);
        let commands: Vec<_> = ids.iter().map(|id| command(id)).collect();
        let field = |c: &lightagent_protocol_conformance::Value, name: &str| {
            let lightagent_protocol_conformance::Value::Object(m) = c else {
                panic!()
            };
            m.iter().find(|(k, _)| k == name).unwrap().1.clone()
        };
        for name in [
            "protocol",
            "purpose",
            "operation",
            "jobId",
            "requestId",
            "audience",
        ] {
            assert!(
                commands
                    .iter()
                    .all(|c| field(c, name) == field(&commands[0], name)),
                "{name} must repeat"
            );
        }
        for c in &commands {
            let payload = field(c, "payload");
            assert_eq!(jcs(&payload), jcs(&field(&commands[0], "payload")));
            let operation = match field(c, "operation") {
                lightagent_protocol_conformance::Value::String(o) => o,
                _ => panic!(),
            };
            assert_eq!(
                payload_hash(&operation, &payload),
                s(&logical["payloadHash"])
            );
        }
        for name in ["commandId", "nonce", "issuedAt", "expiresAt", "signature"] {
            let distinct: BTreeSet<Vec<u8>> =
                commands.iter().map(|c| jcs(&field(c, name))).collect();
            assert_eq!(distinct.len(), commands.len(), "{name} must be renewed");
        }
    }

    // The counter-example: same job and key, different payload.
    let not = &doc["notARedelivery"];
    let create = &doc["logicalCommands"][0];
    assert_eq!(create["operation"], "profile.create");
    let changed = parse_strict(
        serde_json::to_string(&not["command"]["payload"])
            .unwrap()
            .as_bytes(),
    )
    .unwrap();
    let hash = payload_hash("profile.create", &changed);
    assert_eq!(hash, s(&not["payloadHash"]));
    assert_ne!(hash, s(&create["payloadHash"]));
    assert_eq!(not["command"]["jobId"], create["jobId"]);
    let first = s(&create["transmissions"][0]);
    assert_eq!(
        not["command"]["payload"]["idempotencyKey"],
        vectors[first]["detail"]["command"]["payload"]["idempotencyKey"]
    );
    assert_eq!(not["expectedHarnessCode"], "IDEMPOTENCY_CONFLICT");
}
