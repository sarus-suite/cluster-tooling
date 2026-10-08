use crate::{DriverError, Result};
#[cfg(target_os = "linux")]
use std::path::PathBuf;

const CPU_LIST_ORIGIN: &str = "Linux CPU list";
#[cfg(any(target_os = "linux", test))]
const STATUS_FIELD: &str = "Cpus_allowed_list:";
#[cfg(any(target_os = "linux", test))]
const STATUS_PATH: &str = "/proc/thread-self/status";

/// A validated Linux CPU list suitable for Podman's `--cpuset-cpus` option.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CpuSet {
    list: String,
}

impl CpuSet {
    /// Constructs a CPU set from the Linux comma-separated CPU-list format.
    pub fn from_linux_list(list: &str) -> Result<Self> {
        let trimmed = list.trim();
        if trimmed.is_empty() {
            return Err(invalid_list(list, "CPU list is empty"));
        }

        let mut previous_end = None;
        for component in trimmed.split(',') {
            if component.is_empty() {
                return Err(invalid_list(list, "CPU list contains an empty component"));
            }

            let (start, end) = match component.split_once('-') {
                Some((start, end)) => {
                    if end.contains('-') {
                        return Err(invalid_list(
                            list,
                            format!(
                                "component `{component}` contains more than one range separator"
                            ),
                        ));
                    }
                    let start = parse_cpu_id(start, list, component)?;
                    let end = parse_cpu_id(end, list, component)?;
                    if start > end {
                        return Err(invalid_list(
                            list,
                            format!("range `{component}` starts after it ends"),
                        ));
                    }
                    (start, end)
                }
                None => {
                    let cpu = parse_cpu_id(component, list, component)?;
                    (cpu, cpu)
                }
            };

            if previous_end.is_some_and(|previous_end| start <= previous_end) {
                return Err(invalid_list(
                    list,
                    format!("component `{component}` overlaps or is out of order"),
                ));
            }
            previous_end = Some(end);
        }

        Ok(Self {
            list: trimmed.to_owned(),
        })
    }

    /// Reads this Linux thread's allowed CPU list from procfs.
    pub fn from_current_thread() -> Result<Self> {
        #[cfg(target_os = "linux")]
        {
            let status =
                std::fs::read_to_string(STATUS_PATH).map_err(|source| DriverError::File {
                    operation: "read calling thread CPU affinity status",
                    path: PathBuf::from(STATUS_PATH),
                    source,
                })?;
            parse_status(&status)
        }

        #[cfg(not(target_os = "linux"))]
        {
            Err(DriverError::InvalidOutput {
                origin: String::from("Linux CPU affinity"),
                message: String::from("current-thread CPU affinity is supported only on Linux"),
                output: String::new(),
            })
        }
    }

    /// Returns the validated CPU list in Linux textual form.
    pub fn as_str(&self) -> &str {
        &self.list
    }
}

fn parse_cpu_id(value: &str, input: &str, component: &str) -> Result<u32> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid_list(
            input,
            format!("component `{component}` must contain ASCII decimal CPU IDs"),
        ));
    }

    value.parse::<u32>().map_err(|_| {
        invalid_list(
            input,
            format!("CPU ID in component `{component}` exceeds the u32 range"),
        )
    })
}

fn invalid_list(input: &str, message: impl Into<String>) -> DriverError {
    DriverError::InvalidOutput {
        origin: String::from(CPU_LIST_ORIGIN),
        message: message.into(),
        output: input.to_owned(),
    }
}

#[cfg(any(target_os = "linux", test))]
fn parse_status(status: &str) -> Result<CpuSet> {
    let mut values = status
        .lines()
        .filter_map(|line| line.strip_prefix(STATUS_FIELD));
    let Some(value) = values.next() else {
        return Err(DriverError::InvalidOutput {
            origin: String::from(STATUS_PATH),
            message: format!("missing `{STATUS_FIELD}` field"),
            output: status.to_owned(),
        });
    };
    if values.next().is_some() {
        return Err(DriverError::InvalidOutput {
            origin: String::from(STATUS_PATH),
            message: format!("duplicate `{STATUS_FIELD}` fields"),
            output: status.to_owned(),
        });
    }

    CpuSet::from_linux_list(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_and_preserves_valid_cpu_lists() {
        for (input, expected) in [
            ("0", "0"),
            ("0,64", "0,64"),
            ("0,1", "0,1"),
            ("0-3,64-67", "0-3,64-67"),
            ("1024,4096-4097", "1024,4096-4097"),
        ] {
            let cpus = CpuSet::from_linux_list(input).unwrap();
            assert_eq!(cpus.as_str(), expected);
        }

        assert_eq!(
            CpuSet::from_linux_list("  0,64 \n").unwrap().as_str(),
            "0,64"
        );
    }

    #[test]
    fn rejects_malformed_cpu_lists() {
        for input in [
            "",
            " \t\n",
            "0,,2",
            "-1",
            "+1",
            "2-1",
            "1-2-3",
            "a",
            "0, 2",
            "4294967296",
            "0-4294967296",
            "1,1",
            "0-2,2-4",
            "64,0",
        ] {
            let error = CpuSet::from_linux_list(input).unwrap_err();
            assert!(
                matches!(error, DriverError::InvalidOutput { .. }),
                "unexpected error for {input:?}: {error}"
            );
            assert!(error.to_string().contains("CPU list"));
        }
    }

    #[test]
    fn extracts_cpu_list_from_synthetic_status() {
        let status = "Name:\ttest\nState:\tR\nCpus_allowed_list:\t0,64\nMems_allowed_list:\t0\n";
        assert_eq!(parse_status(status).unwrap().as_str(), "0,64");
    }

    #[test]
    fn rejects_missing_duplicate_and_invalid_status_cpu_lists() {
        for status in [
            "Name:\ttest\n",
            "Cpus_allowed_list:\t0\nCpus_allowed_list:\t1\n",
            "Cpus_allowed_list:\t0,,1\n",
        ] {
            assert!(matches!(
                parse_status(status),
                Err(DriverError::InvalidOutput { .. })
            ));
        }
    }
}
