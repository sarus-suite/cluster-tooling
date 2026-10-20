//! String formatting for collected inventory reports. No store access or
//! external tools are needed. Display caches remain content-unverified.
use crate::{ListEntry, ListReport, ListStatus, inventory::display_path};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

const NONE: &str = "<none>";

/// Format Podman-style columns, optionally inserting the full stored-root
/// digest. IMAGE ID is the first 12 hex characters of the OCI config digest.
/// SIZE is stored content size, which differs from Podman's unpacked size.
pub fn format_list_podman(report: &ListReport, show_digests: bool) -> String {
    format_podman_at(report, show_digests, OffsetDateTime::now_utc())
}

fn created(entry: &ListEntry) -> Option<OffsetDateTime> {
    let value = entry.metadata.as_ref()?.content.created.as_deref()?;
    OffsetDateTime::parse(value, &Rfc3339).ok()
}

fn names(entry: &ListEntry) -> (String, String) {
    match &entry.local_ref {
        Some(reference) => (reference.name().into(), reference.tag().into()),
        None => (display_path(&entry.path), NONE.into()),
    }
}

fn format_podman_at(report: &ListReport, show_digests: bool, now: OffsetDateTime) -> String {
    let mut headings = vec!["REPOSITORY", "TAG"];
    if show_digests {
        headings.push("DIGEST");
    }
    headings.extend(["IMAGE ID", "CREATED", "SIZE"]);
    let mut rows = vec![headings.into_iter().map(String::from).collect()];
    let mut entries: Vec<_> = report.entries.iter().collect();
    entries.sort_by(|a, b| {
        created(b)
            .cmp(&created(a))
            .then(a.local_ref.cmp(&b.local_ref))
            .then(a.path.cmp(&b.path))
    });
    for entry in entries {
        let (repository, tag) = names(entry);
        let mut row = vec![repository, tag];
        if show_digests {
            row.push(
                entry
                    .stored_root
                    .as_ref()
                    .map_or_else(|| NONE.into(), ToString::to_string),
            );
        }
        row.push(entry.metadata.as_ref().map_or_else(
            || NONE.into(),
            |m| m.content.config_digest.hex()[..12].into(),
        ));
        row.push(created(entry).map_or_else(
            || NONE.into(),
            |created| {
                age(now
                    .unix_timestamp()
                    .saturating_sub(created.unix_timestamp())
                    .max(0) as u64)
            },
        ));
        row.push(
            entry
                .metadata
                .as_ref()
                .map_or_else(|| NONE.into(), |m| human_size(m.content.size_bytes)),
        );
        rows.push(row);
    }
    table(rows)
}

/// Rich inventory columns with full identities, cached timestamps, exact
/// bytes and status. Diagnostics are carried by the report, not this table.
pub fn format_list_full(report: &ListReport) -> String {
    let mut rows = vec![
        vec![
            "REPOSITORY",
            "TAG",
            "IMAGE ID",
            "DIGEST",
            "ACQUIRED",
            "CREATED",
            "PLATFORM",
            "SIZE BYTES",
            "STATUS",
            "PATH",
        ]
        .into_iter()
        .map(String::from)
        .collect(),
    ];
    for entry in &report.entries {
        let (repository, tag) = names(entry);
        let metadata = entry.metadata.as_ref();
        let acquired = metadata
            .and_then(|m| i64::try_from(m.acquired_at_unix_seconds).ok())
            .and_then(|seconds| OffsetDateTime::from_unix_timestamp(seconds).ok())
            .and_then(|date| date.format(&Rfc3339).ok())
            .unwrap_or_else(|| NONE.into());
        rows.push(vec![
            repository,
            tag,
            metadata.map_or_else(|| NONE.into(), |m| m.content.config_digest.to_string()),
            entry
                .stored_root
                .as_ref()
                .map_or_else(|| NONE.into(), ToString::to_string),
            acquired,
            metadata
                .and_then(|m| m.content.created.clone())
                .unwrap_or_else(|| NONE.into()),
            metadata.map_or_else(
                || NONE.into(),
                |m| {
                    let p = &m.content.platform;
                    let mut value = format!("{}/{}", p.os, p.architecture);
                    if let Some(variant) = &p.variant {
                        value.push('/');
                        value.push_str(variant);
                    }
                    value
                },
            ),
            metadata.map_or_else(|| NONE.into(), |m| m.content.size_bytes.to_string()),
            match entry.status {
                ListStatus::Unverified => "unverified",
                ListStatus::Damaged => "damaged",
            }
            .into(),
            display_path(&entry.path),
        ]);
    }
    table(rows)
}

// Podman uses go-units HumanDuration and appends " ago". Keep its thresholds,
// including rounding hours and using 30-day months and 365-day years.
fn age(seconds: u64) -> String {
    let value = match seconds {
        0 => "Less than a second".into(),
        1 => "1 second".into(),
        2..60 => format!("{seconds} seconds"),
        60..120 => "About a minute".into(),
        120..3600 => format!("{} minutes", seconds / 60),
        _ => {
            let hours = seconds.saturating_add(1800) / 3600;
            match hours {
                0 | 1 => "About an hour".into(),
                2..48 => format!("{hours} hours"),
                48..336 => format!("{} days", hours / 24),
                336..1440 => format!("{} weeks", hours / 168),
                1440..17520 => format!("{} months", hours / 720),
                _ => format!("{} years", seconds / 31_536_000),
            }
        }
    };
    format!("{value} ago")
}

fn human_size(bytes: u64) -> String {
    let units = ["B", "kB", "MB", "GB", "TB", "PB", "EB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < units.len() {
        value /= 1000.0;
        unit += 1;
    }
    // go-units uses %.*g with three significant digits. Scaled nonzero
    // values are at least one; rounded 1000 uses its scientific spelling.
    let precision = if value == 0.0 {
        0
    } else {
        (2 - value.log10().floor() as i32).max(0) as usize
    };
    let mut number = format!("{value:.precision$}");
    if number.parse::<f64>().is_ok_and(|rounded| rounded >= 1000.0) {
        number = "1e+03".into();
    } else if number.contains('.') {
        number = number.trim_end_matches('0').trim_end_matches('.').into();
    }
    format!("{number} {}", units[unit])
}

fn table(mut rows: Vec<Vec<String>>) -> String {
    // Prevent metadata or malformed paths from adding rows or terminal control
    // sequences. Names and digests already use restricted ASCII grammars.
    for row in &mut rows {
        for cell in row {
            *cell = cell
                .chars()
                .flat_map(|c| {
                    if c.is_control() {
                        c.escape_default().collect::<Vec<_>>()
                    } else {
                        vec![c]
                    }
                })
                .collect();
        }
    }
    let columns = rows[0].len();
    let widths: Vec<_> = (0..columns)
        .map(|i| rows.iter().map(|row| row[i].chars().count()).max().unwrap())
        .collect();
    let mut output = String::new();
    for row in rows {
        for (i, value) in row.iter().enumerate() {
            output.push_str(value);
            if i + 1 < columns {
                output.extend(std::iter::repeat_n(
                    ' ',
                    widths[i] - value.chars().count() + 2,
                ));
            }
        }
        output.push('\n');
    }
    output
}

/// Human-readable checking results; diagnostics are printed separately on stderr.
pub fn format_check(report: &crate::CheckReport) -> String {
    use std::fmt::Write;
    let mut text = String::new();
    let mut passed = 0;
    for entry in &report.entries {
        let status = if entry.status == crate::CheckStatus::Passed {
            passed += 1;
            "passed"
        } else {
            "failed"
        };
        let name = entry
            .local_ref
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| crate::inventory::display_path(&entry.path));
        writeln!(text, "{name}: {status}").expect("writing to a String cannot fail");
    }
    writeln!(
        text,
        "Checked {} entries: {passed} passed, {} failed; {} diagnostics; {}",
        report.entries.len(),
        report.entries.len() - passed,
        report.diagnostics.len(),
        if report.complete {
            "complete"
        } else {
            "incomplete"
        }
    )
    .expect("writing to a String cannot fail");
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LocalRef, RefMetadata, Sha256Digest};

    fn entry(name: &str, timestamp: Option<&str>) -> ListEntry {
        let reference = LocalRef::new(name).unwrap();
        let root = Sha256Digest::new(format!("sha256:{}", "d".repeat(64))).unwrap();
        let mut metadata = RefMetadata::new(reference.clone(), root.clone());
        metadata.content.config_digest =
            Sha256Digest::new(format!("sha256:{}", "c".repeat(64))).unwrap();
        metadata.content.created = timestamp.map(String::from);
        metadata.content.size_bytes = 1_234_567;
        ListEntry {
            path: format!("/store/{name}").into(),
            local_ref: Some(reference),
            stored_root: Some(root),
            metadata: Some(metadata),
            status: ListStatus::Unverified,
        }
    }
    fn report(entries: Vec<ListEntry>) -> ListReport {
        ListReport {
            entries,
            diagnostics: Vec::new(),
            complete: true,
        }
    }
    fn now() -> OffsetDateTime {
        OffsetDateTime::parse("2026-01-01T01:01:00Z", &Rfc3339).unwrap()
    }

    #[test]
    fn podman_columns_match_exactly_with_and_without_digests() {
        let report = report(vec![entry("tool:t", Some("2026-01-01T00:00:00Z"))]);
        assert_eq!(
            format_podman_at(&report, false, now()),
            concat!(
                "REPOSITORY  TAG  IMAGE ID      CREATED            SIZE\n",
                "tool        t    cccccccccccc  About an hour ago  1.23 MB\n",
            )
        );
        let expected = format!(
            "{:<12}{:<5}{:<73}{:<14}{:<19}{}\n{:<12}{:<5}{:<73}{:<14}{:<19}{}\n",
            "REPOSITORY",
            "TAG",
            "DIGEST",
            "IMAGE ID",
            "CREATED",
            "SIZE",
            "tool",
            "t",
            format!("sha256:{}", "d".repeat(64)),
            "cccccccccccc",
            "About an hour ago",
            "1.23 MB",
        );
        assert_eq!(format_podman_at(&report, true, now()), expected);
        assert!(
            !expected.contains(&"a".repeat(12)),
            "source provenance ID leaked"
        );
    }

    #[test]
    fn empty_reports_have_headers_and_a_final_newline() {
        let report = report(vec![]);
        assert_eq!(
            format_podman_at(&report, false, now()),
            "REPOSITORY  TAG  IMAGE ID  CREATED  SIZE\n"
        );
        assert_eq!(
            format_podman_at(&report, true, now()),
            "REPOSITORY  TAG  DIGEST  IMAGE ID  CREATED  SIZE\n"
        );
    }

    #[test]
    fn relative_ages_match_podman_boundaries() {
        for (seconds, expected) in [
            (0, "Less than a second ago"),
            (1, "1 second ago"),
            (59, "59 seconds ago"),
            (60, "About a minute ago"),
            (119, "About a minute ago"),
            (120, "2 minutes ago"),
            (3599, "59 minutes ago"),
            (3600, "About an hour ago"),
            (5399, "About an hour ago"),
            (5400, "2 hours ago"),
            (170999, "47 hours ago"),
            (171000, "2 days ago"),
            (1_207_800, "2 weeks ago"),
            (5_182_200, "2 months ago"),
            (63_070_200, "1 years ago"),
            (63_072_000, "2 years ago"),
        ] {
            assert_eq!(age(seconds), expected, "{seconds}");
        }
    }

    #[test]
    fn decimal_sizes_use_three_significant_digits() {
        for (bytes, expected) in [
            (0, "0 B"),
            (1, "1 B"),
            (999, "999 B"),
            (1000, "1 kB"),
            (1234, "1.23 kB"),
            (999499, "999 kB"),
            (999999, "1e+03 kB"),
            (1_000_000, "1 MB"),
            (1_234_567, "1.23 MB"),
            (123_456_789, "123 MB"),
            (u64::MAX, "18.4 EB"),
        ] {
            assert_eq!(human_size(bytes), expected);
        }
    }

    #[test]
    fn ordering_missing_times_long_names_and_damaged_paths_stay_readable() {
        let long = format!("{}:tag", "long/".repeat(20).trim_end_matches('/'));
        let mut damaged = entry("damaged:t", None);
        damaged.local_ref = None;
        damaged.path = "/store/unknown\npath".into();
        damaged.metadata = None;
        damaged.stored_root = None;
        damaged.status = ListStatus::Damaged;
        let report = report(vec![
            entry("old:t", Some("2025-01-01T00:00:00Z")),
            entry("missing:t", None),
            entry("badtime:t", Some("invalid")),
            entry(&long, Some("2026-01-01T01:00:00.123+00:00")),
            entry("aaa:t", Some("2026-01-01T02:00:00.123+01:00")),
            damaged,
        ]);
        let output = format_podman_at(&report, false, now());
        let lines: Vec<_> = output.lines().collect();
        assert_eq!(lines.len(), 7);
        assert!(lines[1].starts_with("aaa "));
        assert!(lines[2].starts_with(long.split_once(':').unwrap().0));
        assert!(lines[3].starts_with("old "));
        assert!(output.contains("/store/unknown\\npath"));
        assert!(lines[1].ends_with("About a minute ago  1.23 MB"));
        for line in lines.iter().skip(1).take(3) {
            assert_eq!(line.find("cccccccccccc"), lines[0].find("IMAGE ID"));
        }
        let full = format_list_full(&report);
        assert!(
            full.contains("SIZE BYTES") && full.contains("1234567") && full.contains("damaged")
        );
        assert!(full.contains("1970-01-01T00:00:00Z"));
        assert!(full.contains(&format!("sha256:{}", "c".repeat(64))));
        assert!(full.contains("linux/amd64"));
    }
}
