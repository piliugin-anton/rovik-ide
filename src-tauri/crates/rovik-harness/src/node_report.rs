//! Node's JUnit dialect carries its complete run totals in trailing XML comments.
use crate::{
    normalizer::{attributes, finish_case, Inventory},
    require, wire, Error, Result,
};
use quick_xml::{events::Event, Reader};
use std::collections::BTreeMap;

pub(crate) fn parse(bytes: &[u8]) -> Result<Inventory> {
    require(
        bytes.len() <= wire::MAX_JSON_BYTES,
        "Node report exceeds limit",
    )?;
    let text =
        std::str::from_utf8(bytes).map_err(|_| Error::new("Node report is not UTF-8".into()))?;
    let mut reader = Reader::from_str(text);
    reader.config_mut().check_end_names = true;
    let mut stack: Vec<Vec<u8>> = Vec::new();
    let mut inventory = Inventory {
        schema_version: 1,
        ..Default::default()
    };
    let mut test = None;
    let mut roots = 0;
    let mut suites = 0;
    let mut totals = BTreeMap::new();
    let mut duration = false;
    loop {
        let event = reader
            .read_event()
            .map_err(|e| Error::new(format!("malformed Node JUnit: {e}")))?;
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let empty = matches!(event, Event::Empty(_));
                let name = e.name().as_ref().to_vec();
                let attr = attributes(e)?;
                require(
                    stack.len() < 64 && totals.is_empty(),
                    "Node report nesting or trailing data error",
                )?;
                match name.as_slice() {
                    b"testsuites" => {
                        require(
                            roots == 0 && stack.is_empty() && !empty,
                            "duplicate or empty Node root",
                        )?;
                        roots += 1;
                    }
                    b"testsuite" | b"testcase" => {
                        require(
                            stack
                                .last()
                                .is_some_and(|v| v == b"testsuite" || v == b"testsuites")
                                && test.is_none(),
                            "misplaced Node test or suite",
                        )?;
                        let id = attr
                            .get("name")
                            .ok_or_else(|| Error::new("Node test has no name".into()))?;
                        crate::id(id)?;
                        if name == b"testsuite" {
                            suites += 1;
                        } else {
                            test = Some((id.clone(), 0));
                        }
                    }
                    b"failure" | b"error" | b"skipped" => {
                        require(
                            stack.last().is_some_and(|v| v == b"testcase"),
                            "misplaced Node outcome",
                        )?;
                        let (_, status) = test
                            .as_mut()
                            .ok_or_else(|| Error::new("Node outcome outside test".into()))?;
                        require(*status == 0, "conflicting Node outcomes")?;
                        *status = if name == b"skipped" { 2 } else { 1 };
                    }
                    _ => return Err(Error::new("unsupported Node JUnit element".into())),
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
                require(stack.pop() == Some(name.clone()), "unbalanced Node report")?;
                if name == b"testcase" {
                    finish_case(&mut inventory, &mut test)?;
                }
            }
            Event::Comment(e) => {
                require(stack == [b"testsuites".to_vec()], "misplaced Node summary")?;
                let line = e.decode().map_err(|e| Error::new(e.to_string()))?;
                let (key, value) = line
                    .trim()
                    .split_once(' ')
                    .ok_or_else(|| Error::new("malformed Node total".into()))?;
                if key == "duration_ms" {
                    let millis: f64 = value
                        .parse()
                        .map_err(|_| Error::new("invalid Node duration".into()))?;
                    require(
                        !duration && millis.is_finite() && millis >= 0.0,
                        "invalid Node duration",
                    )?;
                    duration = true;
                } else {
                    require(
                        [
                            "tests",
                            "suites",
                            "pass",
                            "fail",
                            "cancelled",
                            "skipped",
                            "todo",
                        ]
                        .contains(&key),
                        "unknown Node summary category",
                    )?;
                    let n: usize = value
                        .parse()
                        .map_err(|_| Error::new("invalid Node count".into()))?;
                    require(
                        totals.insert(key.to_string(), n).is_none(),
                        "duplicate Node count",
                    )?;
                }
            }
            Event::Text(e) => {
                let text = e.xml_content().map_err(|e| Error::new(e.to_string()))?;
                require(
                    test.is_some() || text.trim().is_empty(),
                    "text outside Node test",
                )?;
            }
            Event::GeneralRef(e) => {
                let reference = e.decode().map_err(|e| Error::new(e.to_string()))?;
                require(
                    test.is_some()
                        && (e.is_char_ref()
                            || ["amp", "lt", "gt", "apos", "quot"].contains(&reference.as_ref())),
                    "unknown Node XML entity",
                )?;
                if e.is_char_ref() {
                    e.resolve_char_ref()
                        .map_err(|e| Error::new(e.to_string()))?;
                }
            }
            Event::Decl(_) if roots == 0 => {}
            Event::CData(_) if test.is_some() => {}
            Event::Eof => break,
            _ => {
                return Err(Error::new(
                    "unsupported Node XML declaration or instruction".into(),
                ))
            }
        }
    }
    require(
        roots == 1 && stack.is_empty() && test.is_none() && totals.len() == 7 && duration,
        "truncated Node test report",
    )?;
    require(
        totals["tests"] == inventory.selected.len()
            && totals["suites"] == suites
            && totals["pass"] == inventory.passed.len()
            && totals["fail"] == inventory.failed.len()
            && totals["cancelled"] == 0
            && totals["skipped"].checked_add(totals["todo"]) == Some(inventory.skipped.len()),
        "Node summary disagrees with cases or execution was cancelled",
    )?;
    inventory.validate()?;
    Ok(inventory)
}

#[cfg(test)]
mod tests {
    use super::*;
    const REPORT: &str = r#"<?xml version="1.0"?><testsuites><testcase name="good"/><testcase name="bad"><failure message="x">a &lt; b</failure></testcase><testcase name="skip"><skipped/></testcase><!-- tests 3 --><!-- suites 0 --><!-- pass 1 --><!-- fail 1 --><!-- cancelled 0 --><!-- skipped 1 --><!-- todo 0 --><!-- duration_ms 1.2 --></testsuites>"#;
    #[test]
    fn node_totals_and_complete_xml_are_required_even_after_successful_cases() {
        let report = parse(REPORT.as_bytes()).unwrap();
        assert_eq!(report.passed, ["good"]);
        assert_eq!(report.failed, ["bad"]);
        assert_eq!(report.skipped, ["skip"]);
        for bad in [
            REPORT.replace("tests 3", "tests 4"),
            REPORT.replace("<!-- fail 1 -->", ""),
            REPORT.replace("cancelled 0", "cancelled 1"),
            REPORT.replace("</testsuites>", ""),
            REPORT.replace("name=\"bad\"", "name=\"good\""),
            format!("{REPORT}{REPORT}"),
            REPORT.replace("&lt;", "&external;"),
        ] {
            assert!(parse(bad.as_bytes()).is_err(), "accepted {bad}");
        }
    }
}
