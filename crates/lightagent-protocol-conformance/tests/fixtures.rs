//! Runs Web's candidate `LA-WEB-PROVISIONING/1` fixtures, byte for byte as
//! mirrored under `docs/integrations/web/la-web-provisioning-1-r4/`, through
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
/// `la-web-provisioning-1/r4`, committed in Web `f70789e` and delivered as a
/// verified `git bundle` (r1 `030537b8…`, r2 `8649b453…` and r3 `bd410d38…`
/// are superseded).
const COMPATIBILITY_DIGEST: &str =
    "454066f8fb75e886119c401e4a45ec15ebf2b83115165b2e1f85180abfd29a43";
const FIXTURES: &str = "protocol/la-web-provisioning-1";

fn mirror() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/integrations/web/la-web-provisioning-1-r4")
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
    assert_eq!(manifest["fixtureSet"], "la-web-provisioning-1/r4");

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
        let list: Vec<String> = ["vectors", "cases", "deadlineCases", "races", "scenarios"]
            .iter()
            .filter_map(|key| file.get(*key).and_then(Json::as_array))
            .flatten()
            .map(|x| s(&x["id"]).to_owned())
            .collect();
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

// --- r2: Web-side conclusions, execution deadline, harness races ------------

use lightagent_protocol_conformance::model::{self, Attempt, Guard, RaceResult, Rng};
use lightagent_protocol_conformance::web::{
    self, Classified, Outcome, RegisteredCode, Registry, Request, Resolution, Transmission,
};

fn web_registry() -> Registry {
    let doc = json("errors.json");
    Registry {
        codes: doc["errors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                (
                    s(&e["code"]).to_owned(),
                    RegisteredCode {
                        http: e["http"].as_u64().unwrap() as u16,
                        class: s(&e["class"]).to_owned(),
                        operations: e["operations"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|o| s(o).to_owned())
                            .collect(),
                    },
                )
            })
            .collect(),
        failure_codes: doc["operationFailureCodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| s(c).to_owned())
            .collect(),
    }
}

fn valid_command(id: &str) -> Json {
    json("commands-valid.json")["vectors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == id)
        .unwrap_or_else(|| panic!("no command {id}"))["detail"]["command"]
        .clone()
}

fn to_json(value: &lightagent_protocol_conformance::Value) -> Json {
    serde_json::from_slice(&jcs(value)).unwrap()
}

/// §12.4 and §13.1 over all 45 `responses.json` vectors.
#[test]
fn every_response_is_classified_as_expected() {
    let registry = web_registry();
    let mut failures = Vec::new();
    let vectors = json("responses.json")["vectors"]
        .as_array()
        .unwrap()
        .clone();
    for vector in &vectors {
        let id = s(&vector["id"]);
        let command = valid_command(s(&vector["request"]));
        let request = Request {
            command_id: s(&command["commandId"]).to_owned(),
            audience: s(&command["audience"]).to_owned(),
            operation: s(&command["operation"]).to_owned(),
            profile_id: s(&command["payload"]["profileId"]).to_owned(),
            operation_id: command["payload"]["operationId"]
                .as_str()
                .map(str::to_owned),
        };
        let got = match web::classify_response(
            &registry,
            &request,
            vector["httpStatus"].as_u64().unwrap() as u16,
            s(&vector["body"]).as_bytes(),
        ) {
            Classified::Operation {
                result,
                replayed,
                ownership,
            } => serde_json::json!({
                "kind": "operation",
                "result": to_json(&result),
                "replayed": replayed,
                "ownership": ownership,
            }),
            Classified::Refused { code, class } => {
                serde_json::json!({"kind": "refused", "code": code, "class": class})
            }
            Classified::Uncertain { reason } => {
                serde_json::json!({"kind": "uncertain", "reason": reason})
            }
        };
        if got != vector["expect"] {
            failures.push(format!("{id}: expected {}, got {got}", vector["expect"]));
        }
    }
    println!(
        "responses.json: {}/{} as expected",
        vectors.len() - failures.len(),
        vectors.len()
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert_eq!(vectors.len(), 45);
}

fn resolution_json(resolution: &Resolution) -> Json {
    match resolution {
        Resolution::Unresolved => serde_json::json!({"status": "unresolved"}),
        Resolution::Resolved {
            operation_id,
            state,
        } => serde_json::json!({"status": "resolved", "operationId": operation_id, "state": state}),
        Resolution::Failed { code } => serde_json::json!({"status": "failed", "code": code}),
        Resolution::Attention { code } => serde_json::json!({"status": "attention", "code": code}),
    }
}

/// §13.2 R1–R3 over all 21 `create-resolution.json` cases.
#[test]
fn every_create_resolution_case_is_reproduced() {
    let registry = web_registry();
    let cases = json("create-resolution.json")["cases"]
        .as_array()
        .unwrap()
        .clone();
    let mut failures = Vec::new();
    for case in &cases {
        let transmissions: Vec<Transmission> = case["transmissions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| Transmission {
                issued_at: t["issuedAt"].as_u64().unwrap(),
                expires_at: t["expiresAt"].as_u64().unwrap(),
                outcome: match s(&t["outcome"]["kind"]) {
                    "operation" => Outcome::Operation {
                        operation_id: s(&t["outcome"]["operationId"]).to_owned(),
                        state: s(&t["outcome"]["state"]).to_owned(),
                    },
                    "refused" => Outcome::Refused {
                        code: s(&t["outcome"]["code"]).to_owned(),
                    },
                    "uncertain" => Outcome::Uncertain,
                    other => panic!("outcome {other}"),
                },
            })
            .collect();
        let resolution = web::resolve_create_job(&registry, &transmissions);
        let got = resolution_json(&resolution);
        let plan = serde_json::json!({
            "present": web::plan(&resolution, true),
            "absent": web::plan(&resolution, false),
            "retiredPresent": web::plan_retired(&resolution, true),
            "retiredAbsent": web::plan_retired(&resolution, false),
        });
        if plan != case["plan"] {
            failures.push(format!(
                "{}: plan {}, got {plan}",
                s(&case["id"]),
                case["plan"]
            ));
        }
        if got != case["expect"] {
            failures.push(format!(
                "{}: expected {}, got {got}",
                s(&case["id"]),
                case["expect"]
            ));
        }
    }
    println!(
        "create-resolution.json: {}/{} as expected",
        cases.len() - failures.len(),
        cases.len()
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert_eq!(cases.len(), 29);
}

fn race_attempts(scenario: &Json) -> Vec<Attempt> {
    let quota: Vec<(u64, u64)> = scenario["quotaAvailable"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| (w[0].as_u64().unwrap(), w[1].as_u64().unwrap()))
        .collect();
    let mut attempts: Vec<Attempt> = scenario["attempts"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(index, a)| {
            let admitted = a["admittedAt"].as_u64().unwrap();
            let lock = a["lockAcquiredAt"].as_u64().unwrap_or(admitted);
            Attempt {
                index,
                issued_at: a["issuedAt"].as_u64().unwrap(),
                expires_at: a["expiresAt"].as_u64().unwrap(),
                admitted_clock: admitted,
                lock_clock: lock,
                quota_free: quota.iter().any(|(from, to)| (*from..=*to).contains(&lock)),
                response_lost: a["responseLost"].as_bool().unwrap_or(false),
                crash_after_record: a["crashAfterRecord"].as_bool().unwrap_or(false),
            }
        })
        .collect();
    // The fixtures' clock is a correct harness clock, so lock order is
    // clock order.
    attempts.sort_by_key(|a| (a.lock_clock, a.index));
    attempts
}

fn race_json(result: &RaceResult) -> Json {
    serde_json::json!({
        "harness": result.harness,
        "operation": result.operation,
        "web": resolution_json(&result.web),
        "unsafe": result.unsafe_,
    })
}

/// All 10 `execution-deadline.json` cases: the 5 single step-14 checks, and
/// the 5 races with and without step 14. The proposed fence (RC5) gives the
/// same answers as step 14 alone on every race, so it costs no liveness on a
/// correct clock.
#[test]
fn every_execution_deadline_case_is_reproduced() {
    let doc = json("execution-deadline.json");
    let mut checked = 0;
    for case in doc["deadlineCases"].as_array().unwrap() {
        let command = valid_command(s(&case["command"]));
        let expires = command["expiresAt"].as_u64().unwrap();
        assert_eq!(
            case["executionDeadline"].as_u64().unwrap(),
            expires + 30,
            "{}",
            case["id"]
        );
        assert!(case["admittedAt"].as_u64().unwrap() < expires + 30);
        // r3 §10 step 14: the wall clock and the durable fence.
        let fence = case["fence"].as_u64().unwrap();
        let wall = model::execution_deadline(expires, case["lockAcquiredAt"].as_u64().unwrap());
        let got = if wall == "EXPIRED" || expires + 30 <= fence {
            "EXPIRED"
        } else {
            "proceed"
        };
        assert_eq!(got, s(&case["expect"]), "{}", case["id"]);
        checked += 1;
    }
    let registry = web_registry();
    for race in doc["races"].as_array().unwrap() {
        let id = s(&race["id"]);
        let attempts = race_attempts(&race["scenario"]);
        let with = race_json(&model::run(&registry, &attempts, Guard::Deadline));
        assert_eq!(with, race["withExecutionDeadline"], "{id} with step 14");
        let without = race_json(&model::run(&registry, &attempts, Guard::None));
        assert_eq!(
            without, race["withoutExecutionDeadline"],
            "{id} without step 14"
        );
        let fenced = race_json(&model::run(&registry, &attempts, Guard::DeadlineAndFence));
        assert_eq!(fenced, race["withExecutionDeadline"], "{id} with the fence");
        checked += 1;
    }
    println!("execution-deadline.json: {checked}/13 as expected");
    assert_eq!(checked, 13);
}

/// Seeded random schedules of one job (up to 4 transmissions, skew within
/// ±30 s, network delay, lock waits, lost answers, crashes after recording,
/// a quota window). With a harness clock that never steps, step 14 makes R3
/// safe, and without it R3 is unsafe.
#[test]
fn step_14_makes_r3_safe_on_a_clock_that_never_steps() {
    let registry = web_registry();
    let mut rng = Rng::new(0x5eed_0001);
    let (mut without, mut with, mut fenced) = (0, 0, 0);
    const RUNS: usize = 20_000;
    for _ in 0..RUNS {
        let seed = rng.next_u64();
        without +=
            model::random_schedule(&registry, &mut Rng::new(seed), Guard::None, 0).unsafe_ as usize;
        with += model::random_schedule(&registry, &mut Rng::new(seed), Guard::Deadline, 0).unsafe_
            as usize;
        fenced += model::random_schedule(&registry, &mut Rng::new(seed), Guard::DeadlineAndFence, 0)
            .unsafe_ as usize;
    }
    println!(
        "monotone clock, {RUNS} schedules: unsafe without step 14: {without}, with: {with}, with fence: {fenced}"
    );
    assert!(without > 0, "the model must be able to find the r1 race");
    assert_eq!(with, 0);
    assert_eq!(fenced, 0);
}

/// r2 accepts harness clock steps as a residual risk and says a backward
/// step **larger than 30 s** voids R3 (§9.3, §17 item 3). One second is
/// enough. The harness clock runs 30 s behind Web, which r2's skew allows.
/// `t` is issued exactly at `u`'s horizon, runs first, and is refused on
/// the merits at harness time `u.expiresAt + 30`. The clock then steps back
/// 1 s, and `u`, waiting for the lock, starts at `u.expiresAt + 29`. It
/// passes step 14 and binds the user, while Web's R3 has concluded `failed`.
#[test]
fn a_one_second_backward_step_defeats_step_14_but_not_the_fence() {
    let registry = web_registry();
    let u = Attempt {
        index: 0,
        issued_at: 1_791_000_000,
        expires_at: 1_791_000_060,
        admitted_clock: 1_790_999_970,
        lock_clock: 1_791_000_089,
        quota_free: true,
        response_lost: true,
        crash_after_record: false,
    };
    let t = Attempt {
        index: 1,
        issued_at: 1_791_000_120,
        expires_at: 1_791_000_180,
        admitted_clock: 1_791_000_090,
        lock_clock: 1_791_000_090,
        quota_free: false,
        response_lost: false,
        crash_after_record: false,
    };
    // Lock order is real-time order: t, then u.
    let order = [t, u];
    let stepped = model::run(&registry, &order, Guard::Deadline);
    assert_eq!(stepped.harness, ["OPERATION:completed", "QUOTA_EXCEEDED"]);
    assert_eq!(
        stepped.web,
        Resolution::Failed {
            code: "QUOTA_EXCEEDED".to_owned()
        }
    );
    assert!(
        stepped.unsafe_,
        "Web says failed; the user owns the profile"
    );

    let fenced = model::run(&registry, &order, Guard::DeadlineAndFence);
    assert_eq!(fenced.harness, ["EXPIRED", "QUOTA_EXCEEDED"]);
    assert_eq!(fenced.operation, None);
    assert!(!fenced.unsafe_);

    // The hazard randomized within r2's limits (skew ±30 s, lifetimes up to
    // 120 s, clock steps of up to ±90 s, `u` taking the lock at least a
    // second after `t`): step 14 alone is unsafe on many schedules,
    // including backward steps well under the 30 s r2 states; with the
    // fence, none.
    let mut rng = Rng::new(0x5eed_0002);
    let (mut unsafe_steps, mut fenced) = (Vec::new(), 0);
    const PAIRS: usize = 50_000;
    for _ in 0..PAIRS {
        let seed = rng.next_u64();
        let (with, step) = model::stepped_pair(&registry, &mut Rng::new(seed), Guard::Deadline);
        if with.unsafe_ {
            unsafe_steps.push(step);
        }
        let (fence, _) =
            model::stepped_pair(&registry, &mut Rng::new(seed), Guard::DeadlineAndFence);
        fenced += fence.unsafe_ as usize;
    }
    let smallest = unsafe_steps.iter().map(|s| s.abs()).min();
    println!(
        "stepped pairs, {PAIRS}: unsafe with step 14: {} (smallest backward step {smallest:?} s), with fence: {fenced}",
        unsafe_steps.len()
    );
    assert!(unsafe_steps.iter().all(|s| *s < 0), "only backward steps");
    assert!(smallest.is_some_and(|step| step < 30), "{smallest:?}");
    assert_eq!(fenced, 0);

    // Broad random schedules with clock steps: the fence is never unsafe.
    let mut rng = Rng::new(0x5eed_0003);
    const RUNS: usize = 20_000;
    let mut fenced = 0;
    for _ in 0..RUNS {
        fenced += model::random_schedule(&registry, &mut rng, Guard::DeadlineAndFence, 90).unsafe_
            as usize;
    }
    println!("stepped clock, {RUNS} schedules: unsafe with fence: {fenced}");
    assert_eq!(fenced, 0);
}

// --- r3: refusal finality, ordering model, adversarial search --------------

use lightagent_protocol_conformance::ordering::{self, Delivery, Recovery, Rules, Scenario, Tx};

/// `errors.json`'s `createResolution` is exactly §11's class mapping.
#[test]
fn every_code_has_the_finality_its_class_implies() {
    let doc = json("errors.json");
    for e in doc["errors"].as_array().unwrap() {
        assert_eq!(
            s(&e["createResolution"]),
            web::create_resolution(s(&e["class"])),
            "{}",
            e["code"]
        );
    }
}

fn rules(value: &Json) -> Rules {
    let flag = |name: &str| value[name].as_bool().unwrap();
    Rules {
        execution_deadline: flag("executionDeadline"),
        fence: flag("fence"),
        fence_durable_before_unlock: flag("fenceDurableBeforeUnlock"),
        nonce_durable: flag("nonceDurable"),
        nonce_retained_until_fenced: flag("nonceRetainedUntilFenced"),
        protocol_refusals_final: flag("protocolRefusalsFinal"),
        journal_before_send: flag("journalBeforeSend"),
        restore_raises_fence: flag("restoreRaisesFence"),
        ledger_continuity_check: flag("ledgerContinuityCheck"),
        profile_unavailable_rules_out: flag("profileUnavailableRulesOut"),
    }
}

fn u64_list(value: &Json) -> Vec<u64> {
    value
        .as_array()
        .map(|a| a.iter().map(|x| x.as_u64().unwrap()).collect())
        .unwrap_or_default()
}

fn scenario(value: &Json) -> Scenario {
    Scenario {
        transmissions: value["transmissions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| Tx {
                issued_at: t["issuedAt"].as_u64().unwrap(),
                expires_at: t["expiresAt"].as_u64().unwrap(),
                journaled: t["journaled"].as_bool().unwrap_or(true),
            })
            .collect(),
        deliveries: value["deliveries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| Delivery {
                transmission: d["transmission"].as_u64().unwrap() as usize,
                arrives_at: d["arrivesAt"].as_u64().unwrap(),
                lock_at: d["lockAt"].as_u64(),
                hold_for: d["holdFor"].as_u64().unwrap_or(1),
                answer_to_web: d["answerToWeb"].as_bool().unwrap_or(true),
                crash_after_record: d["crashAfterRecord"].as_bool().unwrap_or(false),
            })
            .collect(),
        clock: value["harnessClock"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|c| (c["from"].as_u64().unwrap(), c["offset"].as_i64().unwrap()))
                    .collect()
            })
            .unwrap_or_default(),
        quota: value["quota"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| (w[0].as_u64().unwrap(), w[1].as_u64().unwrap()))
            .collect(),
        restarts: u64_list(&value["restarts"]),
        housekeeping: u64_list(&value["housekeeping"]),
        key_trusted_from: value["keyTrustedFrom"].as_u64().unwrap_or(0),
        recoveries: value["recoveries"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|r| {
                        let has = |store: &str| {
                            r["restore"].as_array().unwrap().iter().any(|x| x == store)
                        };
                        Recovery {
                            at: r["at"].as_u64().unwrap(),
                            ledger: has("ledger"),
                            identity: has("identity"),
                            nonces: has("nonces"),
                            fence: has("fence"),
                            fence_to: r["fenceTo"].as_u64().unwrap_or(0),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn run_json(run: &ordering::Run) -> Json {
    serde_json::json!({
        "answers": run.answers,
        "operation": run.operation.as_ref().map(|(state, by)| serde_json::json!({"state": state, "recordedBy": by})),
        "binding": run.binding.map(|by| serde_json::json!({"createdBy": by})),
        "failedClosed": run.failed_closed,
        "web": resolution_json(&run.web),
        "failedAt": run.failed_at,
        "unsafe": run.unsafe_,
    })
}

/// All 17 `ordering-model.json` scenarios under all 14 rule sets, through
/// the independent model: answers, operation, surviving binding, whether
/// the gateway failed closed, Web's conclusion, when it concluded `failed`,
/// and whether that was unsafe.
#[test]
fn every_ordering_scenario_is_reproduced_under_every_rule_set() {
    let registry = web_registry();
    let doc = json("ordering-model.json");
    let rule_sets = doc["ruleSets"].as_object().unwrap();
    let scenarios = doc["scenarios"].as_array().unwrap();
    let mut failures = Vec::new();
    let mut evaluated = 0;
    for case in scenarios {
        let id = s(&case["id"]);
        let model = scenario(&case["scenario"]);
        for (name, flags) in rule_sets {
            let got = run_json(&ordering::simulate(&registry, &model, rules(flags)));
            if got != case["results"][name] {
                failures.push(format!(
                    "{id} / {name}: expected {}, got {got}",
                    case["results"][name]
                ));
            }
            evaluated += 1;
        }
    }
    println!(
        "ordering-model.json: {} scenarios × {} rule sets, {}/{evaluated} as expected",
        scenarios.len(),
        rule_sets.len(),
        evaluated - failures.len()
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert_eq!((scenarios.len(), rule_sets.len()), (17, 14));
}

/// The review's own search (independent of Web's seeds and generator):
/// copies, clock offsets and steps, restarts, nonce housekeeping, late keys,
/// lock waits, long holds, lost answers, crashes after recording and
/// unjournaled sends. Full r4 (and r3, outside recoveries) never lets Web
/// conclude `failed` while an operation or binding exists. Each ablation
/// that the broad search can reach does.
#[test]
fn r4_holds_on_random_adversarial_schedules_and_each_rule_is_needed() {
    let registry = web_registry();
    let doc = json("ordering-model.json");
    const RUNS: usize = 40_000;
    let mut counts = BTreeMap::new();
    for (name, flags) in doc["ruleSets"].as_object().unwrap() {
        let rules = rules(flags);
        let mut rng = Rng::new(0x1a5e_0003);
        let mut violations = 0;
        for _ in 0..RUNS {
            let model = ordering::random_scenario(&mut rng, false);
            violations += ordering::simulate(&registry, &model, rules).violates_s() as usize;
        }
        counts.insert(name.clone(), violations);
    }
    println!("random adversarial schedules ({RUNS} each): {counts:?}");
    for safe in [
        "r4",
        "r3",
        "r3-without-wall-clock-deadline",
        "r4-without-continuity-check",
    ] {
        assert_eq!(counts[safe], 0, "{safe}");
    }
    // Broad schedules reach these ablations. The fence, its durability
    // before unlock and nonce retention need narrow hazards: the fixtures
    // and `a_one_second_backward_step_defeats_step_14_but_not_the_fence`
    // cover them (Web's broad search shows the same split, §18).
    for ablation in [
        "r2",
        "reviewProposal",
        "r3-without-durable-nonces",
        "r3-protocol-refusals-final",
        "r3-without-journal",
    ] {
        assert!(
            counts[ablation] > 0,
            "{ablation} should be unsafe somewhere"
        );
    }
}

/// Recovery family (the review's own generator, `random_recovery_scenario`):
/// one create, one recovery of a random non-empty subset of {ledger,
/// identity, nonces, fence}, then resends at or after the horizon. r4 is
/// never unsafe. r3, and r4 without its continuity check, are. Broad
/// schedules with a recovery are added to the same check.
#[test]
fn r4_holds_under_partial_recoveries() {
    let registry = web_registry();
    let doc = json("ordering-model.json");
    const RUNS: usize = 40_000;
    let mut counts = BTreeMap::new();
    for name in [
        "r4",
        "r3",
        "r4-without-continuity-check",
        "r4-profile-unavailable-rules-out",
        "r4-restore-without-fence-raise",
    ] {
        let rules = rules(&doc["ruleSets"][name]);
        let mut rng = Rng::new(0x1a5e_0004);
        let mut violations = 0;
        for _ in 0..RUNS {
            let model = ordering::random_recovery_scenario(&mut rng);
            violations += ordering::simulate(&registry, &model, rules).violates_s() as usize;
        }
        counts.insert(name, violations);
    }
    println!("random recovery schedules ({RUNS} each): {counts:?}");
    assert_eq!(counts["r4"], 0);
    let mut rng = Rng::new(0x1a5e_0005);
    let broad = (0..RUNS)
        .filter(|_| {
            let model = ordering::random_scenario(&mut rng, true);
            ordering::simulate(&registry, &model, Rules::R4).violates_s()
        })
        .count();
    println!("broad schedules with a recovery under r4 ({RUNS}): {broad} unsafe");
    assert_eq!(broad, 0);
    assert!(counts["r3"] > 0);
    assert!(counts["r4-without-continuity-check"] > 0);
}

/// The r3 counterexample (RC7), deterministically, plus the variants the
/// brief names that `ordering-model.json` does not already state: under r4
/// the gateway fails closed, no binding is altered, and Web never concludes
/// `failed`.
#[test]
fn rc7_counterexamples_fail_closed_under_r4() {
    let registry = web_registry();
    const T0: u64 = 1_791_000_000;
    let job = |quota_until: u64, recovery: Recovery, u_crashes: bool| Scenario {
        transmissions: vec![
            Tx {
                issued_at: T0,
                expires_at: T0 + 60,
                journaled: true,
            },
            Tx {
                issued_at: T0 + 400,
                expires_at: T0 + 460,
                journaled: true,
            },
            Tx {
                issued_at: T0 + 500,
                expires_at: T0 + 560,
                journaled: true,
            },
        ],
        deliveries: vec![
            Delivery {
                transmission: 0,
                arrives_at: T0 + 1,
                lock_at: None,
                hold_for: 1,
                answer_to_web: false,
                crash_after_record: u_crashes,
            },
            Delivery {
                transmission: 1,
                arrives_at: T0 + 401,
                lock_at: None,
                hold_for: 1,
                answer_to_web: true,
                crash_after_record: false,
            },
            Delivery {
                transmission: 2,
                arrives_at: T0 + 501,
                lock_at: None,
                hold_for: 1,
                answer_to_web: true,
                crash_after_record: false,
            },
        ],
        quota: vec![(0, quota_until)],
        recoveries: vec![recovery],
        ..Scenario::default()
    };
    let at = T0 + 200;
    let fresh = Recovery {
        at,
        ledger: true,
        nonces: true,
        fence: true,
        ..Recovery::default()
    };
    let ledger_only = Recovery {
        at,
        ledger: true,
        ..Recovery::default()
    };
    let identity_only = Recovery {
        at,
        identity: true,
        ..Recovery::default()
    };
    let everything = Recovery {
        at,
        ledger: true,
        identity: true,
        nonces: true,
        fence: true,
        fence_to: 0,
    };
    for (name, model) in [
        (
            "fresh ledger, kept identity",
            job(u64::MAX, fresh.clone(), false),
        ),
        ("fresh ledger, quota refusal", job(T0 + 50, fresh, false)),
        (
            "older ledger, newer identity",
            job(u64::MAX, ledger_only.clone(), false),
        ),
        (
            "older identity, newer ledger",
            job(u64::MAX, identity_only, false),
        ),
        // The lost ledger had only `u`'s pending record: nothing is bound,
        // so the resend may create the profile.
        ("lost ledger, pending u", job(u64::MAX, ledger_only, true)),
        ("consistent restore", job(u64::MAX, everything, false)),
    ] {
        let r3 = ordering::simulate(&registry, &model, Rules::R3);
        let r4 = ordering::simulate(&registry, &model, Rules::R4);
        println!(
            "{name}: r3 {:?} unsafe={} | r4 {:?} failed_closed={}",
            r3.web, r3.unsafe_, r4.web, r4.failed_closed
        );
        assert!(!r4.unsafe_, "{name}");
        assert!(
            !matches!(r4.web, Resolution::Failed { .. }) || r4.binding.is_none(),
            "{name}"
        );
        if r4.failed_closed {
            assert_eq!(
                r4.binding,
                r3.binding.or(r4.binding),
                "{name}: no binding altered"
            );
            assert!(
                r4.answers[1..]
                    .iter()
                    .all(|a| a == "LEDGER_CONTINUITY_FAILED"),
                "{name}"
            );
            assert!(matches!(r4.web, Resolution::Attention { .. }), "{name}");
        }
    }
    // r3 is unsafe on the first two, as reported.
    assert!(
        ordering::simulate(
            &registry,
            &job(
                u64::MAX,
                Recovery {
                    at,
                    ledger: true,
                    nonces: true,
                    fence: true,
                    ..Recovery::default()
                },
                false
            ),
            Rules::R3
        )
        .unsafe_
    );
}

/// §10.6 C1–C3 over all 13 `ledger-continuity.json` cases.
#[test]
fn every_ledger_continuity_case_is_reproduced() {
    use lightagent_protocol_conformance::continuity::{self, LedgerState};
    let doc = json("ledger-continuity.json");
    let strings = |v: &Json| -> Vec<String> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|x| s(x).to_owned())
            .collect()
    };
    let cases = doc["cases"].as_array().unwrap();
    for case in cases {
        let input = &case["input"];
        let ledger = (!input["ledger"].is_null()).then(|| LedgerState {
            harness_id: input["ledger"]["harnessId"].as_str().map(str::to_owned),
            retired: input["ledger"]["retired"].as_bool().unwrap(),
            operations: input["ledger"]["operations"]
                .as_array()
                .unwrap()
                .iter()
                .map(|o| (s(&o["operationId"]).to_owned(), s(&o["state"]).to_owned()))
                .collect(),
        });
        let got = match continuity::check(
            s(&input["configuredHarnessId"]),
            ledger.as_ref(),
            &strings(&input["identityProvisioningRecords"]),
            &strings(&input["profileMarkers"]),
        ) {
            Ok(()) => serde_json::json!({"serve": true}),
            Err(reason) => serde_json::json!({"serve": false, "reason": reason}),
        };
        assert_eq!(got, case["expect"], "{}", case["id"]);
    }
    println!(
        "ledger-continuity.json: {}/{} as expected",
        cases.len(),
        cases.len()
    );
    assert_eq!(cases.len(), 13);
}

/// `LEDGER_CONTINUITY_FAILED` is a 503, `protocol`, `transmission-only` code:
/// never evidence, and never a failed job.
#[test]
fn ledger_continuity_failed_is_never_evidence() {
    let registry = web_registry();
    let entry = &registry.codes["LEDGER_CONTINUITY_FAILED"];
    assert_eq!((entry.http, entry.class.as_str()), (503, "protocol"));
    assert_eq!(web::create_resolution(&entry.class), "transmission-only");
    let t = |issued_at: u64, outcome: Outcome| Transmission {
        issued_at,
        expires_at: issued_at + 60,
        outcome,
    };
    let lcf = || Outcome::Refused {
        code: "LEDGER_CONTINUITY_FAILED".to_owned(),
    };
    // Alone, repeated, or after every other transmission was refused: never failed.
    for journal in [
        vec![t(0, lcf())],
        vec![t(0, lcf()), t(200, lcf()), t(400, lcf())],
        vec![t(0, Outcome::Uncertain), t(200, lcf())],
    ] {
        assert!(!matches!(
            web::resolve_create_job(&registry, &journal),
            Resolution::Failed { .. }
        ));
    }
    // R3a: PROFILE_UNAVAILABLE with an open transmission needs attention,
    // while another merits refusal still rules the same transmission out.
    let pu = Outcome::Refused {
        code: "PROFILE_UNAVAILABLE".to_owned(),
    };
    let quota = Outcome::Refused {
        code: "QUOTA_EXCEEDED".to_owned(),
    };
    assert_eq!(
        web::resolve_create_job(&registry, &[t(0, Outcome::Uncertain), t(200, pu.clone())]),
        Resolution::Attention {
            code: "PROFILE_UNAVAILABLE_WHILE_OPEN".to_owned()
        }
    );
    assert_eq!(
        web::resolve_create_job(&registry, &[t(0, Outcome::Uncertain), t(200, quota)]),
        Resolution::Failed {
            code: "QUOTA_EXCEEDED".to_owned()
        }
    );
    // Without an open transmission, PROFILE_UNAVAILABLE is an ordinary final
    // merits refusal (a profile id genuinely in use).
    assert_eq!(
        web::resolve_create_job(&registry, &[t(0, pu)]),
        Resolution::Failed {
            code: "PROFILE_UNAVAILABLE".to_owned()
        }
    );
}
