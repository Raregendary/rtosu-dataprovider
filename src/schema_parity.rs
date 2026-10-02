//! Schema-level parity drift detection against tosu.
//!
//! Compares JSON payload structures between a reference tosu instance
//! and rtosu-dataprovider across all 4 shapes:
//! - `/json/v2` (ignoring `settings`)
//! - `/json/v2/precise`
//! - `/json/v1` (served by tosu at `/json`)
//! - `/json/sc`

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DriftKind {
    MissingKey,
    UnexpectedKey,
    TypeMismatch { expected: String, actual: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaDrift {
    pub path: String,
    pub kind: DriftKind,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaParityReport {
    pub endpoint: String,
    pub passed: bool,
    pub drifts: Vec<SchemaDrift>,
}

fn type_name(val: &Value) -> &'static str {
    match val {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Recursively compare schemas between `expected` (tosu) and `actual` (rtosu).
///
/// Any key or prefix in `ignore_prefixes` is skipped.
pub fn diff_schema(
    expected: &Value,
    actual: &Value,
    current_path: &str,
    ignore_prefixes: &[&str],
) -> Vec<SchemaDrift> {
    for prefix in ignore_prefixes {
        if current_path == *prefix || current_path.starts_with(&format!("{}.", prefix)) {
            return Vec::new();
        }
    }

    let mut drifts = Vec::new();
    let exp_type = type_name(expected);
    let act_type = type_name(actual);

    // If both are null, types match
    if expected.is_null() && actual.is_null() {
        return drifts;
    }

    // Allow number compatibility (e.g. integer vs float are both numbers)
    if exp_type != act_type {
        // If either is null and the other is a primitive/collection, allow when unpopulated
        if !expected.is_null() && !actual.is_null() {
            drifts.push(SchemaDrift {
                path: if current_path.is_empty() {
                    "root".to_string()
                } else {
                    current_path.to_string()
                },
                kind: DriftKind::TypeMismatch {
                    expected: exp_type.to_string(),
                    actual: act_type.to_string(),
                },
                detail: format!("expected {}, found {}", exp_type, act_type),
            });
            return drifts;
        }
    }

    match (expected, actual) {
        (Value::Object(exp_map), Value::Object(act_map)) => {
            for (key, exp_val) in exp_map {
                let child_path = if current_path.is_empty() {
                    key.clone()
                } else {
                    format!("{}.{}", current_path, key)
                };

                if ignore_prefixes.iter().any(|p| child_path == *p || child_path.starts_with(&format!("{}.", p))) {
                    continue;
                }

                if let Some(act_val) = act_map.get(key) {
                    drifts.extend(diff_schema(exp_val, act_val, &child_path, ignore_prefixes));
                } else {
                    drifts.push(SchemaDrift {
                        path: child_path,
                        kind: DriftKind::MissingKey,
                        detail: format!("key '{}' present in tosu but missing in rtosu", key),
                    });
                }
            }

            for (key, _) in act_map {
                let child_path = if current_path.is_empty() {
                    key.clone()
                } else {
                    format!("{}.{}", current_path, key)
                };

                if ignore_prefixes.iter().any(|p| child_path == *p || child_path.starts_with(&format!("{}.", p))) {
                    continue;
                }

                if !exp_map.contains_key(key) {
                    drifts.push(SchemaDrift {
                        path: child_path,
                        kind: DriftKind::UnexpectedKey,
                        detail: format!("key '{}' present in rtosu but not in tosu", key),
                    });
                }
            }
        }
        (Value::Array(exp_arr), Value::Array(act_arr)) => {
            if let (Some(first_exp), Some(first_act)) = (exp_arr.first(), act_arr.first()) {
                let child_path = format!("{}[0]", current_path);
                drifts.extend(diff_schema(first_exp, first_act, &child_path, ignore_prefixes));
            }
        }
        _ => {}
    }

    drifts
}

/// Fetch a raw HTTP endpoint from tosu.
pub fn fetch_tosu_endpoint(port: u16, path: &str) -> Result<Value> {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .with_context(|| format!("connecting to tosu at 127.0.0.1:{port}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes())?;

    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    let response_str = String::from_utf8_lossy(&response);

    let body = if let Some(pos) = response_str.find("\r\n\r\n") {
        &response_str[pos + 4..]
    } else {
        &response_str
    };

    serde_json::from_str(body).with_context(|| format!("parsing JSON response from tosu {path}"))
}

/// Check schema parity across all 4 endpoints given a live or sampled `TosuV2Packet`.
pub fn check_all_schemas(
    tosu_port: u16,
    packet: &crate::v2::TosuV2Packet,
) -> Result<Vec<SchemaParityReport>> {
    let mut reports = Vec::new();

    // 1. /json/v2 (except settings)
    let tosu_v2 = fetch_tosu_endpoint(tosu_port, "/json/v2")?;
    let rtosu_v2 = serde_json::to_value(packet)?;
    let v2_drifts = diff_schema(&tosu_v2, &rtosu_v2, "", &["settings"]);
    reports.push(SchemaParityReport {
        endpoint: "/json/v2".to_string(),
        passed: v2_drifts.is_empty(),
        drifts: v2_drifts,
    });

    // 2. /json/v2/precise
    let tosu_precise = fetch_tosu_endpoint(tosu_port, "/json/v2/precise")?;
    let rtosu_precise = serde_json::to_value(crate::v2::TosuPrecisePacket::from_v2(packet))?;
    let precise_drifts = diff_schema(&tosu_precise, &rtosu_precise, "", &[]);
    reports.push(SchemaParityReport {
        endpoint: "/json/v2/precise".to_string(),
        passed: precise_drifts.is_empty(),
        drifts: precise_drifts,
    });

    // 3. /json (tosu serves gosumemory v1 on /json)
    let tosu_v1 = fetch_tosu_endpoint(tosu_port, "/json")?;
    let rtosu_v1 = serde_json::to_value(crate::v1::GosuCompatibleApi::from_v2(packet))?;
    let v1_drifts = diff_schema(&tosu_v1, &rtosu_v1, "", &[]);
    reports.push(SchemaParityReport {
        endpoint: "/json/v1".to_string(),
        passed: v1_drifts.is_empty(),
        drifts: v1_drifts,
    });

    // 4. /json/sc
    let tosu_sc = fetch_tosu_endpoint(tosu_port, "/json/sc")?;
    let rtosu_sc = serde_json::to_value(crate::sc::ScPayload::from_v2(packet))?;
    let sc_drifts = diff_schema(&tosu_sc, &rtosu_sc, "", &[]);
    reports.push(SchemaParityReport {
        endpoint: "/json/sc".to_string(),
        passed: sc_drifts.is_empty(),
        drifts: sc_drifts,
    });

    Ok(reports)
}
