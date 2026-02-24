use serde::{Deserialize, Serialize, Serializer};
use std::collections::{HashMap, HashSet};

use crate::common::expand_vars_string;
use crate::error::{SarusError, SarusResult};

pub type SarusMounts = Vec<SarusMount>;

#[derive(Deserialize, Clone, PartialEq)]
pub struct SarusMount {
    source: String,
    target: String,
    flags: String,
}

impl Serialize for SarusMount {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_volume_string())
    }
}

impl SarusMount {

    pub fn to_volume_string(&self) -> String {
        if self.flags.is_empty() {
            format!("{}:{}", self.source, self.target)
        } else {
            format!("{}:{}:{}", self.source, self.target, self.flags)
        }
    }

    pub fn try_new(
        input: String,
        uenv: &Option<HashMap<String, String>>,
    ) -> SarusResult<SarusMount> {

        let mut m = Self::from_string(input)?;
        m.render(uenv)?;
        m.validate()?;

        Ok(m)
    }

    fn from_string(input: String) -> SarusResult<SarusMount> {
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

        let s = a.next().unwrap();
        let t = a.next().unwrap();
        let mut f = "";
        if asize == 3 {
            f = a.next().unwrap();
        }

        let m = SarusMount {
            source: String::from(s),
            target: String::from(t),
            flags: String::from(f),
        };

        Ok(m)
    }

    fn render(
        &mut self,
        uenv: &Option<HashMap<String, String>>,
    ) -> SarusResult<()> {

        let mut i = self.clone();
        i.translate_to_absolute()?;

        let mut s = escape_mount(i.source);
        let mut t = escape_mount(i.target);
        s = expand_vars_string(s, uenv)?;
        t = expand_vars_string(t, uenv)?;

        i.source = s;
        i.target = t;
        i.flags = expand_vars_string(i.flags, uenv)?;
        i.render_flags()?;
        *self = i;

        Ok(())
    }

    fn translate_to_absolute(&mut self) -> SarusResult<()> {

        let mut i = self.clone();

        if i.flags == "sqsh" {
            let mut ps: std::path::PathBuf = std::path::Path::new(&i.source).into();

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
            }

            i.source = match ps.as_os_str().to_str() {
                Some(ok) => ok.to_string(),
                None => {
                    return Err(SarusError {
                        code: 11,
                        file_path: None,
                        msg: format!("cannot translate {} into string", ps.display()),
                    });
                }
            };
        }
        *self = i;

        return Ok(())
    }

    fn render_flags(
        &mut self,
    ) -> SarusResult<()> {

        let mut i = self.clone();

        if i.flags == "sqsh" {
            let metadata = match std::fs::metadata(self.source.as_str()) {
                Ok(m) => m,
                Err(e) => {
                    return Err(SarusError {
                        code: 14,
                        file_path: None,
                        msg: format!("could not stat source of squashfs mount ({}): {}", i.source, e),
                    });
                }
            };
            if !metadata.is_file() {
                return Err(SarusError {
                    code: 16,
                    file_path: None,
                    msg: format!("source of squashfs mount ({}) must be a regular file", i.source),
                });
            }

            i.flags = String::from("");

        } else {

            // Remove duplicate flags
            let parts: Vec<_> = i.flags.split(',').collect();
            let parts_set: HashSet<_> = parts.into_iter().collect();
            let parts_unique_vec: Vec<_> = parts_set.into_iter().collect();
            let f = parts_unique_vec.join(",");
            i.flags = String::from(f);
        }
        *self = i;

        Ok(())
    }

    fn validate(&self) -> SarusResult<()> {

        if ![".", "/"].iter().any(|s| self.source.starts_with(*s)) {
            return Err(SarusError {
                code: 12,
                file_path: None,
                msg: format!(
                    "mount source {:#?} must be one among a relative path starting with . , an absolute path starting with / , \"tmpfs\" or \"umount\"", self.source
                ),
            });
        }

        if ![".", "/"].iter().any(|s| self.target.starts_with(*s)) {
            return Err(SarusError {
                code: 13,
                file_path: None,
                msg: format!(
                    "mount target {:#?} must be one among a relative path starting with . or an absolute path starting with /", self.target
                ),
            });
        }

        return Ok(());
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
