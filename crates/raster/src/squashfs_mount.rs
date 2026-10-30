use std::collections::HashMap;
use serde::{Deserialize, Serialize, Serializer};

use crate::common::expand_vars_string;
use crate::mount::{escape_mount};
use crate::error::{SarusError, SarusResult};

pub type SquashfsMounts = Vec<SquashfsMount>;

#[derive(Deserialize, Clone, PartialEq, Debug)]
pub struct SquashfsMount {
    source: String,
    target: String,
}

impl Serialize for SquashfsMount {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl SquashfsMount {
     pub fn to_string(&self) -> String {
        format!("{}:{}", self.source, self.target)
    }

    pub fn try_new(
        input: String,
        uenv: &Option<HashMap<String, String>>,
    ) -> SarusResult<SquashfsMount> {
        let mut m = Self::from_string(input)?;
        m.render(uenv)?;
        m.validate()?;
        Ok(m)
    }

    fn from_string(input: String) -> SarusResult<SquashfsMount> {
        SquashfsMount::from_edfmount_format_string(input)
    }

    fn from_edfmount_format_string(input: String) -> SarusResult<SquashfsMount> {
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

        match f {
            "sqsh" => {},
            _ => {
                return Err(SarusError {
                    code: 31,
                    file_path: None,
                    msg: format!("Unsupported type in extra mounts"),
                });
            },
        }

        let m = SquashfsMount {
            source: String::from(s),
            target: String::from(t),
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
        *self = i;

        Ok(())
    }

    fn translate_to_absolute(&mut self) -> SarusResult<()> {
        let mut i = self.clone();

        let mut ps: std::path::PathBuf = std::path::Path::new(&i.source).into();

        if ps.starts_with(".") {
            ps = match std::path::absolute(&ps) {
                Err(_) => {
                    return Err(SarusError {
                        code: 39,
                        file_path: None,
                        msg: format!("cannot translate {} in an absolute path", ps.display()),
                    });
                },
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
            },
        };

        let mut pt: std::path::PathBuf = std::path::Path::new(&i.target).into();

        if pt.starts_with(".") {
            pt = match std::path::absolute(&pt) {
                Err(_) => {
                    return Err(SarusError {
                        code: 45,
                        file_path: None,
                        msg: format!("cannot translate {} in an absolute path", pt.display()),
                    });
                },
                Ok(ok) => ok,
            }
        }

        i.target = match pt.as_os_str().to_str() {
            Some(ok) => ok.to_string(),
            None => {
                return Err(SarusError {
                    code: 46,
                    file_path: None,
                    msg: format!("cannot translate {} into string", pt.display()),
                });
            },
        };

        *self = i;

        return Ok(());
    }

    fn validate(&self) -> SarusResult<()> {
        if !self.source.starts_with("/") {
            return Err(SarusError {
                code: 41,
                file_path: None,
                msg: format!(
                    "mount source {:#?} must be an absolute path starting with /",
                    self.source
                ),
            });
        }

        if !self.target.starts_with("/") {
            return Err(SarusError {
                code: 42,
                file_path: None,
                msg: format!(
                    "mount target {:#?} must be an absolute path starting with /",
                    self.target
                ),
            });
        }

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

        return Ok(());
    }

}

pub(crate) trait ToString {
    fn to_string(&self) -> String;
}

impl ToString for SquashfsMounts {
    fn to_string(&self) -> String {
        let mut output = String::from("");
        for mount in self {
            if output.is_empty() {
                output = mount.to_string();
            } else {
                output = format!("{};{}", output, mount.to_string());
            }
        }
        output
    }
}

pub fn squashfs_mounts_from_mount_strings(
    input: Vec<String>,
    uenv: &Option<HashMap<String, String>>,
) -> SarusResult<SquashfsMounts> {
    let mut res = vec![];

    for i in input.iter() {

        let m = match SquashfsMount::try_new(i.clone(), uenv) {
            Ok(sm) => sm,
            Err(e) => {
                match e.code {
                    // Skip Standard mounts -> Error Code: 31
                    // Skip Standard mounts with no flags -> Error Code: 32
                    31 | 32 => {
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
