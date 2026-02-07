use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

use crate::common::expand_vars_string;
use crate::error::{SarusError, SarusResult};

pub type SarusMounts = Vec<SarusMount>;

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct SarusMount {
    source: String,
    target: String,
    flags: String,
}

impl SarusMount {
    pub fn try_new(
        input: String,
        uenv: &Option<HashMap<String, String>>,
    ) -> SarusResult<SarusMount> {
        let mut a = input.split(":");
        let asize = a.clone().count();

        if asize < 2 || asize > 3 {
            return Err(SarusError {
                code: 8,
                file_path: None,
                msg: format!(
                    "{} contains {} number of fields, expected 2 or 3",
                    input, asize
                ),
            });
        };

        let mut s = a.next().unwrap();
        let t = a.next().unwrap();
        let mut f = "";
        if asize == 3 {
            f = a.next().unwrap();
        }

        let mut ps: std::path::PathBuf = std::path::Path::new(s).into();
        let pt: std::path::PathBuf = std::path::Path::new(t).into();

        if f == "sqsh" {
            if ps.starts_with(".") {
                ps = match std::path::absolute(&ps) {
                    Err(_) => {
                        return Err(SarusError {
                            code: 9,
                            file_path: None,
                            msg: format!("cannot translate {} in an absolute path", ps.display()),
                        });
                    }
                    Ok(ok) => ok,
                }
            } else if ps.starts_with("/") {
                ()
            } else {
                return Err(SarusError {
                    code: 10,
                    file_path: None,
                    msg: format!(
                        "source of squashfs mount {} must be a relative path or an absolute path starting with . or /",
                        s
                    ),
                });
            }
            s = match ps.as_os_str().to_str() {
                Some(ok) => ok,
                None => {
                    return Err(SarusError {
                        code: 11,
                        file_path: None,
                        msg: format!("cannot translate {} into string", ps.display()),
                    });
                }
            };
        } else {
            // TODO consider if we should just accept absolute paths
            if ![".", "/"].iter().any(|s| ps.starts_with(*s)) {
                return Err(SarusError {
                    code: 12,
                    file_path: None,
                    msg: format!(
                        "mount source {ps:#?} must be one among a relative path starting with . , an absolute path starting with / , \"tmpfs\" or \"umount\""
                    ),
                });
            }
        }

        if ![".", "/"].iter().any(|s| pt.starts_with(*s)) {
            return Err(SarusError {
                code: 13,
                file_path: None,
                msg: format!(
                    "mount target {pt:#?} must be one among a relative path starting with . or an absolute path starting with /"
                ),
            });
        }

        let mut es = escape_mount(String::from(s));
        let mut et = escape_mount(String::from(t));
        es = expand_vars_string(es, uenv)?;
        et = expand_vars_string(et, uenv)?;

        let em;

        if f == "sqsh" {
            let metadata = match std::fs::metadata(s) {
                Ok(m) => m,
                Err(e) => {
                    return Err(SarusError {
                        code: 14,
                        file_path: None,
                        msg: format!("could not stat source of squashfs mount ({s}): {e}"),
                    });
                }
            };
            if !metadata.is_file() {
                return Err(SarusError {
                    code: 16,
                    file_path: None,
                    msg: format!("source of squashfs mount ({s}) must be a regular file"),
                });
            }

            em = SarusMount {
                source: String::from(es),
                target: String::from(et),
                flags: String::from(""),
            }
        } else {
            let flags: String;
            if f != "" {
                flags = String::from(f);
            } else {
                flags = String::new();
            }
            let flags = expand_vars_string(flags, uenv)?;

            // Remove duplicate flags
            let parts: Vec<_> = flags.split(',').collect();
            let parts_set: HashSet<_> = parts.into_iter().collect();
            let parts_unique_vec: Vec<_> = parts_set.into_iter().collect();
            let ef = parts_unique_vec.join(",");

            em = SarusMount {
                source: String::from(es),
                target: String::from(et),
                flags: String::from(ef),
            }
        }

        return Ok(em);
    }

    pub fn to_volume_string(&self) -> String {
        if self.flags.is_empty() {
            format!("{}:{}", self.source, self.target)
        } else {
            format!("{}:{}:{}", self.source, self.target, self.flags)
        }
    }
}

pub fn sarus_mounts_from_strings(
    input: Vec<String>,
    uenv: &Option<HashMap<String, String>>,
) -> SarusResult<SarusMounts> {
    let mut res = vec![];

    for i in input.iter() {
        let m = SarusMount::try_new(i.clone(), uenv)?;
        if !res.contains(&m) {
            res.push(m.clone());
        }
    }

    Ok(res)
}

// From pyxis code (still needed ???)
// escape source or target mount entry to build an fstab like entry as used by enroot
// from man 3 getmntent:
//     Since fields in the mtab and fstab files are separated by
//     whitespace, octal escapes are used to represent the characters
//     space (\040), tab (\011), newline (\012), and backslash (\\) in
//     those files when they occur in one of the four strings in a
//     mntent structure.  The routines addmntent() and getmntent() will
//     convert from string representation to escaped representation and
//     back.  When converting from escaped representation, the sequence
//     \134 is also converted to a backslash.
fn escape_mount(path: String) -> String {
    let mut epath = String::from("");
    for c1 in path.chars() {
        let c2 = match c1 {
            ' ' => format!("\\040"),
            '\t' => format!("\\011"),
            '\n' => format!("\\012"),
            '\\' => format!("\\\\"),
            _ => format!("{c1}"),
        };
        epath.push_str(c2.as_str());
    }
    epath
}
