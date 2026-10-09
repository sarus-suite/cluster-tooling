use serde::{Deserialize, Serialize, Serializer};
use std::collections::{HashMap, HashSet};

use crate::common::expand_vars_string;
use crate::error::{SarusError, SarusResult};

pub type SarusMounts = Vec<SarusMount>;
pub type SarusExtraMounts = Vec<SarusExtraMount>;

#[derive(Deserialize, Clone, PartialEq, Debug)]
pub struct SarusMount {
    source: String,
    target: String,
    flags: String,
}

#[derive(Deserialize, Clone, PartialEq, Debug)]
pub struct SarusExtraMount {
    kind: SarusExtraMountKind,
    source: String,
    target: String,
    flags: String,
}
#[derive(Deserialize, Clone, PartialEq, Debug)]
pub enum SarusExtraMountKind {
    Squashfs
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

    fn render(&mut self, uenv: &Option<HashMap<String, String>>) -> SarusResult<()> {
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

        return Ok(());
    }

    fn render_flags(&mut self) -> SarusResult<()> {
        let mut i = self.clone();

        if i.flags == "sqsh" {
            let metadata = match std::fs::metadata(self.source.as_str()) {
                Ok(m) => m,
                Err(e) => {
                    return Err(SarusError {
                        code: 14,
                        file_path: None,
                        msg: format!(
                            "could not stat source of squashfs mount ({}): {}",
                            i.source, e
                        ),
                    });
                }
            };
            if !metadata.is_file() {
                return Err(SarusError {
                    code: 16,
                    file_path: None,
                    msg: format!(
                        "source of squashfs mount ({}) must be a regular file",
                        i.source
                    ),
                });
            }

            i.flags = String::from("");

            return Err(SarusError {
                code: 30,
                file_path: None,
                msg: format!("Unsupported SquashFS type for standard podman mounts"),
            });

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
                    "mount source {:#?} must be one among a relative path starting with . , an absolute path starting with / , \"tmpfs\" or \"umount\"",
                    self.source
                ),
            });
        }

        if ![".", "/"].iter().any(|s| self.target.starts_with(*s)) {
            return Err(SarusError {
                code: 13,
                file_path: None,
                msg: format!(
                    "mount target {:#?} must be one among a relative path starting with . or an absolute path starting with /",
                    self.target
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

        let m = match SarusMount::try_new(i.clone(), uenv) {
            Ok(sm) => sm,
            Err(e) => {
                match e.code {
                    // Skip Extra mounts -> Error Code: 30
                    30 => {
                        continue;
                    },
                    _ => {
                        return Err(e);
                    },
                };
            },
        };

        if !res.contains(&m) {
            res.push(m.clone());
        }
    }

    Ok(res)
}

impl Serialize for SarusExtraMount {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl SarusExtraMount {
     pub fn to_string(&self) -> String {

        let kind = match self.kind {
            SarusExtraMountKind::Squashfs => String::from("squashfs"),
        };

        if self.flags.is_empty() {
            format!("type={},src={},dst={}", kind, self.source, self.target)
        } else {
            format!("type={},src={},dst={},{}", kind, self.source, self.target, self.flags)
        }
    }

     pub fn try_new(
        input: String,
        uenv: &Option<HashMap<String, String>>,
    ) -> SarusResult<SarusExtraMount> {
        let mut m = Self::from_string(input)?;
        m.render(uenv)?;
        m.validate()?;
        Ok(m)
    }

    fn from_string(input: String) -> SarusResult<SarusExtraMount> {
        if input.starts_with("type=") {
            SarusExtraMount::_from_type_format_string(input)
        } else {
            SarusExtraMount::_from_colon_format_string(input)
        }
    }

    fn _from_colon_format_string(input: String) -> SarusResult<SarusExtraMount> {
        let mut a = input.split(":");
        let asize = a.clone().count();

        if asize != 3 {
            return Err(SarusError {
                code: 32,
                file_path: None,
                msg: format!(
                    "{} contains {} number of fields, expected 3",
                    input, asize
                ),
            });
        };

        let s = a.next().unwrap();
        let t = a.next().unwrap();
        let f = a.next().unwrap();

        let k = match f {
            "sqsh" => SarusExtraMountKind::Squashfs,
            _ => {
                return Err(SarusError {
                    code: 31,
                    file_path: None,
                    msg: format!("Unsupported type in extra podman mounts"),
                });
            },
        };

        let m = SarusExtraMount {
            kind: k,
            source: String::from(s),
            target: String::from(t),
            flags: String::from(""),
        };

        Ok(m)
    }

    fn _from_type_format_string(input: String) -> SarusResult<SarusExtraMount> {
        let a = input.split(",");
        let asize = a.clone().count();

        if asize < 3 {
            return Err(SarusError {
                code: 33,
                file_path: None,
                msg: format!(
                    "{} contains {} number of fields, minimum is 3",
                    input, asize
                ),
            });
        }

        let mut kind: Option<SarusExtraMountKind> = None;
        let mut source: Option<String> = None;
        let mut destination: Option<String> = None;
        let mut options: Option<String> = None;

        for x in a {
            match x {
                s if s.starts_with("type=") => {
                    let typestr = s.strip_prefix("type=").unwrap_or("");
                    match typestr {
                        "squashfs" => { 
                            kind = Some(SarusExtraMountKind::Squashfs);
                        },
                        _ => {
                            return Err(SarusError {
                                code: 31,
                                file_path: None,
                                msg: format!("Unsupported type in extra podman mounts"),
                            });
                        },
                    }
                },
                s if s.starts_with("source=") => {
                    source = parse_mount_field(s, source.is_some(), "source", "source")?;
                },
                s if s.starts_with("src=") => {
                    source = parse_mount_field(s, source.is_some(), "source", "src")?;
                },
                s if s.starts_with("destination=") => {
                    destination = parse_mount_field(s, destination.is_some(), "destination", "destination")?;
                },
                s if s.starts_with("dst=") => {
                    destination = parse_mount_field(s, destination.is_some(), "destination", "dst")?;
                },
                s if s.starts_with("target=") => {
                    destination = parse_mount_field(s, destination.is_some(), "destination", "target")?;
                },
                _ => {
                    match options {
                        None => {
                            options = Some(x.to_string());
                        },
                        Some(v) => {
                            let new_value = format!("{v},{x}");
                            options = Some(new_value);
                        },
                    }
                },

            }
        }

        if kind.is_none() {
            return Err(SarusError {
                code: 38,
                file_path: None,
                msg: format!("Missing type in extra podman mounts"),
            });
        }

        if source.is_none() {
            return Err(SarusError {
                code: 39,
                file_path: None,
                msg: format!("Missing source in extra podman mounts"),
            });
        }

        if destination.is_none() {
            return Err(SarusError {
                code: 39,
                file_path: None,
                msg: format!("Missing destination in extra podman mounts"),
            });
        }
        
        if options.is_none() {
            options = Some(String::from(""));
        }
        
        let m = SarusExtraMount {
            kind: kind.unwrap(),
            source: source.unwrap(),
            target: destination.unwrap(),
            flags: options.unwrap(),
        };

        Ok(m)
    }

    fn render(&mut self, uenv: &Option<HashMap<String, String>>) -> SarusResult<()> {
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

    fn render_flags(&mut self) -> SarusResult<()> {
        let mut i = self.clone();

        let parts: Vec<_> = i.flags.split(',').collect();
        let parts_set: HashSet<_> = parts.into_iter().collect();
        let parts_unique_vec: Vec<_> = parts_set.into_iter().collect();
        let f = parts_unique_vec.join(",");
        i.flags = String::from(f);
        *self = i;

        Ok(())
    }

    fn translate_to_absolute(&mut self) -> SarusResult<()> {
        let mut i = self.clone();

        if i.kind == SarusExtraMountKind::Squashfs {
            let mut ps: std::path::PathBuf = std::path::Path::new(&i.source).into();

            if ps.starts_with(".") {
                ps = match std::path::absolute(&ps) {
                    Err(_) => {
                        return Err(SarusError {
                            code: 39,
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
                        code: 40,
                        file_path: None,
                        msg: format!("cannot translate {} into string", ps.display()),
                    });
                }
            };
        }
        *self = i;

        return Ok(());
    }

    fn validate(&self) -> SarusResult<()> {
        if ![".", "/"].iter().any(|s| self.source.starts_with(*s)) {
            return Err(SarusError {
                code: 41,
                file_path: None,
                msg: format!(
                    "mount source {:#?} must be one among a relative path starting with . , an absolute path starting with / , \"tmpfs\" or \"umount\"",
                    self.source
                ),
            });
        }

        if ![".", "/"].iter().any(|s| self.target.starts_with(*s)) {
            return Err(SarusError {
                code: 42,
                file_path: None,
                msg: format!(
                    "mount target {:#?} must be one among a relative path starting with . or an absolute path starting with /",
                    self.target
                ),
            });
        }

        if self.kind == SarusExtraMountKind::Squashfs {
            let metadata = match std::fs::metadata(self.source.as_str()) {
                Ok(m) => m,
                Err(e) => {
                    return Err(SarusError {
                        code: 43,
                        file_path: None,
                        msg: format!(
                            "could not stat source of squashfs mount ({}): {}",
                            self.source, e
                        ),
                    });
                }
            };

            if !metadata.is_file() {
                return Err(SarusError {
                    code: 44,
                    file_path: None,
                    msg: format!(
                        "source of squashfs mount ({}) must be a regular file",
                        self.source
                    ),
                });
            }
        }

        return Ok(());
    }

}

pub fn sarus_extra_mounts_from_strings(
    input: Vec<String>,
    uenv: &Option<HashMap<String, String>>,
) -> SarusResult<SarusExtraMounts> {
    let mut res = vec![];

    for i in input.iter() {

        let m = match SarusExtraMount::try_new(i.clone(), uenv) {
            Ok(sm) => sm,
            Err(e) => {
                match e.code {
                    // Skip Standard mounts -> Error Code: 31
                    31 => {
                        continue;
                    },
                    _ => {
                        return Err(e);
                    },
                };
            },
        };

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

fn parse_mount_field(input: &str, some: bool, field: &str, key: &str) -> SarusResult<Option<String>> {

    if some {
        return Err(SarusError {
            code: 35,
            file_path: None,
            msg: format!("Multiple {field}s specified for a single mount"),
        });
    }
    
    let prefix = format!("{key}=");
    let ret = match input.strip_prefix(&prefix) {
        Some(value) => value.to_string(),
        None => {
            return Err(SarusError {
                        code: 36,
                        file_path: None,
                        msg: format!("Unable to parse {field}"),
            });
        },
    };

    if ret.is_empty() {
        return Err(SarusError {
            code: 37,
            file_path: None,
            msg: format!("Unable to parse {field}, empty value"),
        });
    }
    Ok(Some(ret))
}
