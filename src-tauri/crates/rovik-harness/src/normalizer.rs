//! Deterministic native result parsing. Reports remain untrusted observations.

use crate::{evidence::Status, registry::Parser, require, unique, wire, Error, Result};
use quick_xml::{
    events::{BytesStart, Event},
    Reader,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Identity of the selected parser and the exact compiled normalizer source.
/// Protected driver code is bound separately by the oracle manifest.
pub fn checker_digest(parser: Parser) -> crate::identity::Digest {
    crate::identity::Digest::of(
        format!(
            "rovik-normalizer-v1:{parser:?}:{}:{}:{}:{}:{}:{}:{}",
            include_str!("normalizer.rs"),
            include_str!("pack_protocol.rs"),
            include_str!("runtime.rs"),
            include_str!("node_report.rs"),
            include_str!("failure.rs"),
            include_str!("architecture.rs"),
            include_str!("native_checks.rs")
        )
        .as_bytes(),
    )
}

/// Protected driver protocol, including discovery and selection accounting.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inventory {
    /// Exactly 1 for serialized driver reports.
    pub schema_version: u32,
    /// All cases discovered for this check.
    pub discovered: Vec<String>,
    /// Cases selected for execution.
    pub selected: Vec<String>,
    /// Successfully executed cases.
    pub passed: Vec<String>,
    /// Executed cases violating their contract.
    pub failed: Vec<String>,
    /// Ignored, filtered or skipped selected cases.
    pub skipped: Vec<String>,
}
impl Inventory {
    /// Reject competing outcomes and any executed case absent from discovery/selection.
    pub fn validate(&self) -> Result<()> {
        require(self.schema_version == 1, "unsupported inventory version")?;
        for group in [
            &self.discovered,
            &self.selected,
            &self.passed,
            &self.failed,
            &self.skipped,
        ] {
            unique(group.iter().map(String::as_str))?;
        }
        let discovered: BTreeSet<_> = self.discovered.iter().collect();
        let selected: BTreeSet<_> = self.selected.iter().collect();
        require(
            selected.is_subset(&discovered),
            "selected case absent from discovery",
        )?;
        let mut outcomes = BTreeSet::new();
        for case in self.passed.iter().chain(&self.failed).chain(&self.skipped) {
            require(outcomes.insert(case), "case has conflicting outcomes")?;
        }
        require(
            outcomes == selected,
            "selected cases have missing or unselected outcomes",
        )
    }
    /// Number of cases that executed, excluding skips.
    pub fn executed(&self) -> u64 {
        (self.passed.len() + self.failed.len()) as u64
    }
}

/// Parsed observation; orchestration binds it to a receipt and exact inputs later.
#[derive(Debug, Clone)]
pub struct Normalized {
    /// Native outcome, with parser errors represented explicitly.
    pub status: Status,
    /// Recovery category assigned by the trusted normalizer.
    pub failure_category: Option<crate::FailureCategory>,
    /// Complete native case inventory.
    pub inventory: Inventory,
    /// Explanation of missing/malformed/failed observations.
    pub diagnostic: Option<String>,
}

/// Normalize a completed command. A zero exit never repairs malformed reports.
pub fn normalize(
    parser: Parser,
    exit: Option<i32>,
    stdout: &[u8],
    report: Option<&[u8]>,
    success_codes: &[i32],
) -> Normalized {
    let parsed = match parser {
        Parser::PackProtocol | Parser::PackTests | Parser::PackBuild | Parser::PackNativeTests => {
            Err(Error::new(
                "pack response requires protected request validation".into(),
            ))
        }
        Parser::ExitCode => Ok(Inventory {
            schema_version: 1,
            ..Default::default()
        }),
        Parser::RustTests => parse_rust(stdout),
        Parser::CtestJunit => report
            .ok_or_else(|| Error::new("missing CTest report".into()))
            .and_then(parse_junit),
        Parser::InventoryJson => report
            .ok_or_else(|| Error::new("missing protected driver report".into()))
            .and_then(wire::decode::<Inventory>),
    };
    match parsed.and_then(|inventory| {
        inventory.validate()?;
        Ok(inventory)
    }) {
        Err(error) => Normalized {
            status: Status::Error,
            failure_category: Some(crate::FailureCategory::Infrastructure),
            inventory: Inventory {
                schema_version: 1,
                ..Default::default()
            },
            diagnostic: Some(error.to_string().chars().take(16_384).collect()),
        },
        Ok(inventory) => {
            let status = if !inventory.failed.is_empty()
                || !exit.is_some_and(|c| success_codes.contains(&c))
            {
                Status::Fail
            } else {
                Status::Pass
            };
            Normalized {
                status,
                failure_category: (status != Status::Pass)
                    .then_some(crate::FailureCategory::Implementation),
                inventory,
                diagnostic: None,
            }
        }
    }
}

impl Normalized {
    /// Turn an authenticated adapter failure into a conservative native outcome.
    pub fn from_error(error: &Error) -> Self {
        let category = if error.category == crate::FailureCategory::InvalidInput {
            crate::FailureCategory::Infrastructure
        } else {
            error.category
        };
        Self {
            status: match category {
                crate::FailureCategory::Implementation => Status::Fail,
                crate::FailureCategory::Capability => Status::Unsupported,
                _ => Status::Error,
            },
            failure_category: Some(category),
            inventory: Inventory {
                schema_version: 1,
                ..Default::default()
            },
            diagnostic: Some(error.to_string().chars().take(16_384).collect()),
        }
    }
}

/// Honor only failure-stage markers owned by a registered protected driver.
/// A compiler failure has no behavioral case inventory and cannot validate an oracle.
pub fn normalize_command(
    definition: &crate::registry::CommandDefinition,
    exit: Option<i32>,
    stdout: &[u8],
    report: Option<&[u8]>,
    success_codes: &[i32],
) -> Normalized {
    if exit.is_some_and(|code| definition.implementation_failure_exit_codes.contains(&code)) {
        return Normalized::from_error(&Error::classified(crate::FailureCategory::Implementation,
            "protected driver reported a candidate compilation failure; inspect the native diagnostics"));
    }
    normalize(definition.parser, exit, stdout, report, success_codes)
}

/// Parse a single stable libtest binary, retaining required ignored tests.
pub fn parse_rust(bytes: &[u8]) -> Result<Inventory> {
    require(
        bytes.len() <= wire::MAX_JSON_BYTES,
        "Rust report exceeds limit",
    )?;
    let text =
        std::str::from_utf8(bytes).map_err(|_| Error::new("Rust report is not UTF-8".into()))?;
    let mut inventory = Inventory {
        schema_version: 1,
        ..Default::default()
    };
    let mut running = None;
    let mut summary = None;
    for line in text.lines().map(str::trim) {
        if let Some(count) = line
            .strip_prefix("running ")
            .and_then(|s| s.strip_suffix(" tests").or_else(|| s.strip_suffix(" test")))
        {
            require(
                running.is_none(),
                "multiple libtest suites need separate registered checks",
            )?;
            running = Some(
                count
                    .parse::<usize>()
                    .map_err(|_| Error::new("malformed Rust discovery count".into()))?,
            );
        } else if let Some(rest) = line
            .strip_prefix("test ")
            .filter(|_| !line.starts_with("test result:"))
        {
            if let Some((name, outcome)) = rest.rsplit_once(" ... ") {
                let group = match outcome {
                    "ok" => &mut inventory.passed,
                    "FAILED" => &mut inventory.failed,
                    _ if outcome == "ignored" || outcome.starts_with("ignored,") => {
                        &mut inventory.skipped
                    }
                    _ => return Err(Error::new("unknown Rust test outcome".into())),
                };
                group.push(name.into());
                inventory.discovered.push(name.into());
                inventory.selected.push(name.into());
            }
        } else if let Some(rest) = line.strip_prefix("test result: ") {
            require(summary.is_none(), "duplicate Rust test summary")?;
            let (status, counts) = rest
                .split_once(". ")
                .ok_or_else(|| Error::new("malformed Rust summary".into()))?;
            require(
                ["ok", "FAILED"].contains(&status),
                "unknown Rust suite outcome",
            )?;
            let mut totals = BTreeMap::new();
            for field in counts.split(';').take(5) {
                let field = field.trim();
                let (n, kind) = field
                    .split_once(' ')
                    .ok_or_else(|| Error::new("malformed Rust summary count".into()))?;
                let n = n
                    .parse::<usize>()
                    .map_err(|_| Error::new("malformed Rust summary count".into()))?;
                require(
                    totals.insert(kind, n).is_none(),
                    "duplicate Rust summary category",
                )?;
            }
            summary = Some((status, totals));
        }
    }
    let count = running.ok_or_else(|| Error::new("missing Rust test discovery".into()))?;
    let (status, totals) =
        summary.ok_or_else(|| Error::new("missing or truncated Rust test summary".into()))?;
    require(
        count == inventory.selected.len(),
        "Rust discovery differs from execution inventory",
    )?;
    for (kind, actual) in [
        ("passed", inventory.passed.len()),
        ("failed", inventory.failed.len()),
        ("ignored", inventory.skipped.len()),
    ] {
        require(
            totals.get(kind) == Some(&actual),
            format!("inconsistent Rust {kind} count"),
        )?;
    }
    require(
        totals.get("measured") == Some(&0),
        "benchmark results are not behavioral tests",
    )?;
    require(
        (status == "ok") == inventory.failed.is_empty(),
        "Rust suite outcome disagrees with cases",
    )?;
    inventory.validate()?;
    Ok(inventory)
}

pub(crate) fn attributes(event: &BytesStart<'_>) -> Result<BTreeMap<String, String>> {
    let mut fields = BTreeMap::new();
    for attr in event.attributes() {
        let attr = attr.map_err(|e| Error::new(format!("malformed JUnit attribute: {e}")))?;
        let key = std::str::from_utf8(attr.key.as_ref())
            .map_err(|_| Error::new("non-UTF-8 JUnit attribute".into()))?
            .to_string();
        let value = attr
            .unescape_value()
            .map_err(|e| Error::new(format!("invalid JUnit entity: {e}")))?
            .to_string();
        require(
            fields.insert(key, value).is_none(),
            "duplicate JUnit attribute",
        )?;
    }
    Ok(fields)
}

/// Parse CTest JUnit without DTDs or external entities, and require complete counts.
pub fn parse_junit(bytes: &[u8]) -> Result<Inventory> {
    require(
        bytes.len() <= wire::MAX_JSON_BYTES,
        "JUnit report exceeds limit",
    )?;
    let text =
        std::str::from_utf8(bytes).map_err(|_| Error::new("JUnit report is not UTF-8".into()))?;
    let mut reader = Reader::from_str(text);
    reader.config_mut().check_end_names = true;
    let mut inventory = Inventory {
        schema_version: 1,
        ..Default::default()
    };
    let mut stack: Vec<Vec<u8>> = Vec::new();
    let mut test: Option<(String, u8)> = None;
    let mut declared = None;
    let mut suites = 0;
    loop {
        let event = reader
            .read_event()
            .map_err(|e| Error::new(format!("malformed JUnit XML: {e}")))?;
        match event {
            Event::DocType(_) => {
                return Err(Error::new("JUnit DTDs and entities are forbidden".into()))
            }
            Event::Start(ref e) | Event::Empty(ref e) => {
                let empty = matches!(event, Event::Empty(_));
                let name = e.name().as_ref().to_vec();
                require(stack.len() < 64, "JUnit nesting limit exceeded")?;
                let attr = attributes(e)?;
                match name.as_slice() {
                    b"testsuites" => {
                        require(stack.is_empty() && suites == 0, "unexpected JUnit wrapper")?
                    }
                    b"testsuite" => {
                        require(
                            suites == 0
                                && test.is_none()
                                && (stack.is_empty() || stack == [b"testsuites".to_vec()]),
                            "each check requires one complete JUnit suite",
                        )?;
                        suites += 1;
                        let number = |key: &str| -> Result<usize> {
                            attr.get(key)
                                .ok_or_else(|| Error::new(format!("missing JUnit {key}")))?
                                .parse()
                                .map_err(|_| Error::new(format!("invalid JUnit {key}")))
                        };
                        declared = Some((
                            number("tests")?,
                            number("failures")?,
                            attr.get("errors")
                                .map(|s| s.parse())
                                .transpose()
                                .map_err(|_| Error::new("invalid JUnit errors".into()))?
                                .unwrap_or(0),
                            attr.get("skipped")
                                .or_else(|| attr.get("disabled"))
                                .map(|s| s.parse())
                                .transpose()
                                .map_err(|_| Error::new("invalid JUnit skipped".into()))?
                                .unwrap_or(0),
                        ));
                    }
                    b"testcase" => {
                        require(
                            test.is_none() && stack.last().is_some_and(|s| s == b"testsuite"),
                            "nested or misplaced JUnit testcase",
                        )?;
                        let name = attr
                            .get("name")
                            .ok_or_else(|| Error::new("JUnit testcase has no name".into()))?
                            .clone();
                        test = Some((name, 0));
                    }
                    b"failure" | b"error" | b"skipped" => {
                        let (_, status) = test
                            .as_mut()
                            .ok_or_else(|| Error::new("JUnit outcome outside a testcase".into()))?;
                        require(*status == 0, "competing JUnit testcase outcomes")?;
                        *status = if name == b"skipped" {
                            2
                        } else if name == b"error" {
                            3
                        } else {
                            1
                        };
                    }
                    b"system-out" | b"system-err" | b"properties" | b"property" => {}
                    _ => return Err(Error::new("unsupported JUnit element".into())),
                }
                if empty {
                    if name == b"testcase" {
                        finish_case(&mut inventory, &mut test)?;
                    }
                } else {
                    stack.push(name);
                }
            }
            Event::End(e) => {
                let name = e.name().as_ref().to_vec();
                require(
                    stack.pop() == Some(name.clone()),
                    "unbalanced JUnit document",
                )?;
                if name == b"testcase" {
                    finish_case(&mut inventory, &mut test)?;
                }
            }
            Event::Eof => break,
            Event::Text(e) => {
                e.xml_content()
                    .map_err(|_| Error::new("invalid XML text".into()))?;
            }
            Event::GeneralRef(e) => {
                let reference = e.decode().map_err(|e| Error::new(e.to_string()))?;
                require(
                    !stack.is_empty()
                        && (e.is_char_ref()
                            || ["amp", "lt", "gt", "apos", "quot"].contains(&reference.as_ref())),
                    "unknown JUnit XML entity",
                )?;
                if e.is_char_ref() {
                    e.resolve_char_ref()
                        .map_err(|e| Error::new(e.to_string()))?;
                }
            }
            Event::Decl(_) | Event::Comment(_) | Event::CData(_) => {}
            _ => return Err(Error::new("unsupported XML instruction or entity".into())),
        }
    }
    require(
        stack.is_empty() && test.is_none() && suites == 1,
        "truncated or missing JUnit suite",
    )?;
    let (count, failed, errors, skipped) =
        declared.ok_or_else(|| Error::new("missing JUnit counts".into()))?;
    require(
        count == inventory.selected.len()
            && failed.checked_add(errors) == Some(inventory.failed.len())
            && skipped == inventory.skipped.len(),
        "JUnit counts disagree with case outcomes",
    )?;
    inventory.validate()?;
    Ok(inventory)
}

pub(crate) fn finish_case(
    inventory: &mut Inventory,
    test: &mut Option<(String, u8)>,
) -> Result<()> {
    let (name, status) = test
        .take()
        .ok_or_else(|| Error::new("JUnit testcase ended without starting".into()))?;
    inventory.discovered.push(name.clone());
    inventory.selected.push(name.clone());
    match status {
        0 => inventory.passed.push(name),
        2 => inventory.skipped.push(name),
        _ => inventory.failed.push(name),
    }
    Ok(())
}
