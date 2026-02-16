use derivative::Derivative;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::collections::HashSet;
use std::error::Error;
use std::ffi::OsStr;
use std::path::Path;
use toml::Value;
use toml::map::Map;

use crate::common::{expand_vars_hashmap, expand_vars_vec};
use crate::error::{SarusError, SarusResult};
use crate::mount::{SarusMounts, sarus_mounts_from_strings};

pub mod common;
pub mod config;
pub mod error;
pub mod mount;

pub use crate::common::expand_vars_string;
pub use crate::config::{Config, VarExpand, load_config, load_config_path, update_config_by_user};

#[allow(dead_code)]
#[derive(Derivative, Serialize, Deserialize, Clone, Default)]
pub struct RawEDF {
    annotations: Option<Annotations>,
    base_environment: Option<BaseEnvironment>,
    devices: Option<Vec<String>>,
    entrypoint: Option<bool>,
    env: Option<HashMap<String, String>>,
    image: Option<String>,
    mounts: Option<Vec<String>>,
    workdir: Option<String>,
    writable: Option<bool>,
}

#[allow(dead_code)]
#[derive(Derivative, Serialize, Deserialize, Clone)]
pub struct EDF {
    #[serde(default = "get_default_annotations")]
    pub annotations: HashMap<String, String>,
    #[serde(default = "get_default_devices")]
    pub devices: Vec<String>,
    #[serde(default = "get_default_entrypoint")]
    pub entrypoint: bool,
    #[serde(default = "get_default_env")]
    pub env: HashMap<String, String>,
    pub image: String,
    #[serde(default = "get_default_mounts")]
    pub mounts: SarusMounts,
    #[serde(default = "get_default_workdir")]
    pub workdir: String,
    #[serde(default = "get_default_writable")]
    pub writable: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(untagged)]
pub enum BaseEnvironment {
    TypeString(String),
    TypeVec(Vec<String>),
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(untagged)]
pub enum Annotations {
    TypeMap(Map<String, Value>),
    TypeHashMap(HashMap<String, String>),
}

impl RawEDF {
    // Overwrite fields and tables with the other raw EDF.
    fn extend(&mut self, i: RawEDF) {
        if i.annotations.is_some() {
            let i_anno = i.annotations.unwrap();

            let mut self_anno_hm = match &self.annotations {
                Some(self_anno) => annotations_as_hashmap(self_anno.clone()),
                None => HashMap::new(),
            };
            let i_anno_hm = annotations_as_hashmap(i_anno);
            self_anno_hm.extend(i_anno_hm.clone());

            self.annotations = Some(Annotations::TypeHashMap(self_anno_hm));
        }

        if i.devices.is_some() {
            if self.devices.is_some() {
                let i_devices = i.devices.unwrap();
                let self_devices = self.devices.as_mut().unwrap();
                self_devices.extend(i_devices);
            } else {
                self.devices = i.devices;
            }
        }
        if i.env.is_some() {
            if self.env.is_some() {
                let i_env = i.env.unwrap();
                let self_env = self.env.as_mut().unwrap();
                self_env.extend(i_env);
            } else {
                self.env = i.env;
            }
        }
        if i.mounts.is_some() {
            if self.mounts.is_some() {
                let i_mounts = i.mounts.unwrap();
                let self_mounts = self.mounts.as_mut().unwrap();
                self_mounts.extend(i_mounts);
            } else {
                self.mounts = i.mounts;
            }
        }

        if i.entrypoint.is_some() {
            self.entrypoint = i.entrypoint;
        }
        if i.image.is_some() {
            self.image = i.image;
        }
        if i.workdir.is_some() {
            self.workdir = i.workdir;
        }
        if i.writable.is_some() {
            self.writable = i.writable;
        }
    }
}

fn annotations_as_hashmap(a: Annotations) -> HashMap<String, String> {
    let r = match a {
        Annotations::TypeHashMap(h) => h,
        Annotations::TypeMap(m) => map2hashmap(m),
    };
    r
}

fn map2hashmap(m: Map<String, Value>) -> HashMap<String, String> {
    let mut r = HashMap::from([]);
    for i in m.iter() {
        let (k, v) = i;
        if v.is_table() {
            let t = v.as_table().unwrap();
            let h = map2hashmap(t.clone());
            for j in h.iter() {
                let (jk, jv) = j;
                let new_k = format!("{k}.{jk}");
                r.insert(new_k, jv.clone());
            }
        } else {
            if v.is_str() {
                r.insert(k.to_string(), v.as_str().unwrap().to_string());
            } else {
                r.insert(k.to_string(), v.to_string());
            }
        }
    }
    r
}

fn get_default_annotations() -> HashMap<String, String> {
    return HashMap::from([]);
}

fn get_default_devices() -> Vec<String> {
    return vec![];
}

fn get_default_entrypoint() -> bool {
    return true;
}

fn get_default_env() -> HashMap<String, String> {
    return HashMap::from([]);
}

fn get_default_mounts() -> SarusMounts {
    return vec![];
}
fn get_default_workdir() -> String {
    return String::from("");
}

fn get_default_writable() -> bool {
    return true;
}

fn edf_from_raw(r: RawEDF, uenv: &Option<HashMap<String, String>>) -> SarusResult<EDF> {
    Ok(EDF {
        annotations: match r.annotations {
            Some(s) => annotations_as_hashmap(s),
            None => get_default_annotations(),
        },
        devices: match r.devices {
            Some(s) => s,
            None => get_default_devices(),
        },
        entrypoint: match r.entrypoint {
            Some(s) => s,
            None => get_default_entrypoint(),
        },
        env: match r.env {
            Some(s) => s,
            None => get_default_env(),
        },
        image: match r.image {
            Some(s) => s,
            None => {
                return Err(SarusError {
                    code: 7,
                    file_path: None,
                    msg: String::from("missing image specification"),
                });
            }
        },
        mounts: match r.mounts {
            Some(s) => sarus_mounts_from_strings(s, uenv)?,
            None => get_default_mounts(),
        },
        workdir: match r.workdir {
            Some(s) => s,
            None => get_default_workdir(),
        },
        writable: match r.writable {
            Some(s) => s,
            None => get_default_writable(),
        },
    })
}

fn load(file_path: &str) -> Result<String, Box<dyn Error>> {
    // SD-67022 - prevent reading wrong file
    let fp = Path::new(file_path);

    if !fp.exists() {
        return Err(format!("File {file_path} not found").into());
    }

    let outstr = std::fs::read_to_string(file_path)?;

    Ok(outstr)
}

pub(crate) fn check_file_path_extension(file_path: &str, ext: &str) -> SarusResult<()> {
    let fp = Path::new(file_path);

    let fname = match fp.file_name().and_then(OsStr::to_str) {
        Some(name) => name,
        None => {
            return Err(SarusError {
                code: 20,
                file_path: Some(file_path.to_string()),
                msg: String::from("Cannot extract file name"),
            });
        }
    };

    let cur_ext = match fp.extension().and_then(OsStr::to_str) {
        Some(x) => x,
        None => {
            return Err(SarusError {
                code: 21,
                file_path: Some(file_path.to_string()),
                msg: String::from("Cannot extract file extension"),
            });
        }
    };

    if cur_ext != ext {
        return Err(SarusError {
            code: 22,
            file_path: Some(file_path.to_string()),
            msg: format!("File name {fname} doesn't end with .{ext}"),
        });
    }
    Ok(())
}

pub fn validate(path: String) -> SarusResult<()> {
    // Embedding schema file
    let schema_content = include_str!("schema/edf.json");

    check_file_path_extension(&path, "toml")?;

    validate_file(path, schema_content)
}

pub(crate) fn validate_file(path: String, schema_content: &str) -> SarusResult<()> {
    let path_str = path.as_str();

    let schema: serde_json::Value = match serde_json::from_str(&schema_content) {
        Ok(c) => c,
        Err(_) => {
            return Err(SarusError {
                code: 0,
                file_path: None,
                msg: String::from("Failed to parse schema file"),
            });
        }
    };

    let validator = match jsonschema::options().build(&schema) {
        Ok(v) => v,
        Err(error) => {
            return Err(SarusError {
                code: 1,
                file_path: None,
                msg: String::from(format!("Schema is invalid.\n{error}")),
            });
        }
    };

    let mut has_errors = false;

    let toml_in = toml_read(path_str)?;
    let mut errors = validator.iter_errors(&toml_in);
    let mut emsg = String::from("");

    if let Some(first) = errors.next() {
        has_errors = true;
        emsg = format!("Errors:\n1. {first}");
        for (i, error) in errors.enumerate() {
            emsg = String::from(format!("{emsg}\n{}. {}", (i + 2), error));
        }
    }

    if has_errors {
        return Err(SarusError {
            code: 4,
            file_path: Some(String::from(path_str)),
            msg: String::from(format!("{}", emsg)),
        });
    } else {
        return Ok(());
    }
}

pub fn get_search_paths() -> Vec<String> {
    let mut search_paths = vec![];

    let user_sp = get_user_search_paths();
    search_paths.extend(user_sp);

    let sys_sp = get_sys_search_paths();
    search_paths.extend(sys_sp);

    search_paths
}

pub fn get_sys_search_paths() -> Vec<String> {
    let mut search_paths = vec![];

    let config = match load_config() {
        Ok(c) => c,
        Err(_) => {
            return search_paths;
        }
    };

    let sys_search_path = config.edf_system_search_path;

    if sys_search_path != "" {
        let paths = sys_search_path.split(":");
        for p in paths {
            search_paths.push(String::from(p));
        }
    }

    search_paths
}

pub fn get_user_search_paths() -> Vec<String> {
    let mut search_paths = vec![];

    // $EDF_PATH or $HOME/.edf or ""
    let edf_path = match std::env::var("EDF_PATH") {
        Ok(p) => p,
        Err(_) => {
            let home_path = match std::env::var("HOME") {
                Ok(h) => h,
                Err(_) => String::from(""),
            };
            if home_path != "" {
                format!("{home_path}/.edf")
            } else {
                String::from("")
            }
        }
    };
    if edf_path != "" {
        search_paths.push(edf_path);
    }

    search_paths
}

fn resolve_env_path(
    env: String,
    sp: &Vec<String>,
    uenv: &Option<HashMap<String, String>>,
) -> SarusResult<String> {
    let mut retopt = None;
    let mut file_path;

    let ee = expand_vars_string(env, uenv)?;

    // it doesn't look like a file_path
    if ![".", "/"].iter().any(|s| ee.starts_with(*s)) && !ee.ends_with(".toml") {
        for s in sp.iter() {
            file_path = format!("{s}/{ee}.toml");
            if std::path::Path::new(&file_path).is_file() {
                match std::fs::File::open(&file_path) {
                    Ok(_) => {
                        retopt = Some(file_path.clone());
                        break;
                    }
                    Err(_) => continue,
                };
            }
        }
    } else {
        file_path = ee.clone();
        if std::path::Path::new(&file_path).is_file() {
            match std::fs::File::open(&file_path) {
                Ok(_) => {
                    retopt = Some(file_path.clone());
                }
                Err(_) => {}
            }
        }
    }

    match retopt {
        Some(s) => return Ok(s),
        None => {
            let paths = sp
                .iter()
                .map(|x| x.to_string())
                .collect::<Vec<_>>()
                .join(",");
            return Err(SarusError {
                code: 6,
                file_path: None,
                msg: String::from(format!("environment \"{ee}\" not found at {paths}")),
            });
        }
    }
}

pub(crate) fn toml_read<T>(s: &str) -> SarusResult<T>
where
    T: for<'a> Deserialize<'a>,
{
    let toml_content = match load(s) {
        Ok(c) => c,
        Err(e) => {
            return Err(SarusError {
                code: 2,
                file_path: Some(String::from(s)),
                msg: String::from(format!("{}", e)),
            });
        }
    };

    let toml_value = match toml::from_str(toml_content.as_str()) {
        Ok(v) => v,
        Err(e) => {
            return Err(SarusError {
                code: 3,
                file_path: Some(String::from(s)),
                msg: String::from(format!("{}", e)),
            });
        }
    };

    Ok(toml_value)
}

fn render_inner_loop(
    name: String,
    sp: &Vec<String>,
    env: &Option<HashMap<String, String>>,
    mut count: u64,
    max: u64,
) -> SarusResult<RawEDF> {
    count += 1;
    if count > max {
        return Err(SarusError {
            code: 5,
            file_path: None,
            msg: String::from(format!(
                "base_environment rendering has more than {max} levels"
            )),
        });
    }

    let edf_path = resolve_env_path(name.clone(), sp, env)?;
    validate(edf_path.clone())?;

    // Create current raw EDF
    let path_str = edf_path.as_str();
    let mut cur_redf: RawEDF = toml_read(path_str)?;

    // Merge base EDFs
    if cur_redf.base_environment.is_some() {
        let mut base_redf = RawEDF::default();

        let be = cur_redf.base_environment.clone().unwrap();
        let ba = match be {
            BaseEnvironment::TypeString(s) => vec![s],
            BaseEnvironment::TypeVec(a) => a,
        };

        for b in ba.iter() {
            let _base_redf = render_inner_loop(b.to_string(), &sp, env, count, max)?;
            base_redf.extend(_base_redf);
        }
        cur_redf.base_environment = None;

        base_redf.extend(cur_redf);
        cur_redf = base_redf;
    }

    // Expand variables in the fields
    if cur_redf.devices.is_some() {
        cur_redf.devices = Some(expand_vars_vec(cur_redf.devices.unwrap(), env)?);

        // Remove duplicates from devices
        let dev = cur_redf.devices.clone().unwrap();
        let dev_set: HashSet<_> = dev.into_iter().collect();
        let dev_unique_vec: Vec<_> = dev_set.into_iter().collect();
        cur_redf.devices = Some(dev_unique_vec);
    }
    if cur_redf.env.is_some() {
        cur_redf.env = Some(expand_vars_hashmap(cur_redf.env.unwrap(), env)?);
    }
    if cur_redf.annotations.is_some() {
        let a = cur_redf.annotations.unwrap();
        let mut h = annotations_as_hashmap(a);
        h = expand_vars_hashmap(h, env)?;
        cur_redf.annotations = Some(Annotations::TypeHashMap(h));
    }
    if cur_redf.workdir.is_some() {
        cur_redf.workdir = Some(expand_vars_string(cur_redf.workdir.unwrap(), env)?);
    }

    return Ok(cur_redf);
}

pub fn render_from_search_paths(
    path: String,
    search_paths: Vec<String>,
    env: &Option<HashMap<String, String>>,
) -> SarusResult<EDF> {
    let sp = search_paths;
    let max_levels = 10;
    let loop_count = 0;
    let raw = render_inner_loop(path, &sp, env, loop_count, max_levels)?;
    let e = edf_from_raw(raw, env)?;
    Ok(e)
}

pub fn render(path: String) -> SarusResult<EDF> {
    let sp = get_search_paths();
    render_from_search_paths(path, sp, &None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::env;

    pub(crate) fn get_rendered_edf(_edf_filename: &str) -> SarusResult<EDF> {
        let edf_filename = _edf_filename.to_string();
        let old_cwd = match env::current_dir() {
            Ok(_p) => _p,
            Err(_) => panic!("cannot find current working directory"),
        };

        match env::set_current_dir(Path::new("test/toml")) {
            Ok(_) => (),
            Err(_) => panic!(
                "cannot change working directory, cwd: {}",
                old_cwd.display()
            ),
        };

        let result = render(edf_filename.clone());

        match env::set_current_dir(old_cwd) {
            Ok(_) => (),
            Err(_) => panic!("cannot restore working directory"),
        };

        return result;
    }

    #[test]
    #[serial]
    fn render_top_simple() {
        let edf = get_rendered_edf("top-simple-1.toml").unwrap();
        assert!(edf.image == "ubuntu:simple-1");
        assert!(edf.entrypoint == true);
    }

    #[test]
    #[serial]
    fn render_top_devices() {
        let edf = get_rendered_edf("top-devices.toml").unwrap();
        assert!(edf.image == "ubuntu:devices");
        assert!(edf.devices.contains(&"dev1".to_string()));
        assert!(edf.devices.contains(&"dev2".to_string()));
        assert!(edf.devices.contains(&"dev3".to_string()));
        assert!(edf.devices.len() == 3);
    }

    #[test]
    #[serial]
    fn render_top_mounts() {
        let edf = get_rendered_edf("top-mounts.toml").unwrap();
        assert!(edf.image == "ubuntu:mounts");
        assert!(
            edf.mounts
                .iter()
                .any(|e| e.to_volume_string() == "/aaa:/bbb")
        );
        assert!(
            edf.mounts
                .iter()
                .any(|e| e.to_volume_string() == "./ccc:./ddd")
        );
        assert!(
            edf.mounts
                .iter()
                .any(|e| e.to_volume_string() == "/eee:./fff:ggg")
        );
        assert!(edf.mounts.len() == 3);
    }

    #[test]
    #[serial]
    fn render_table_anno() {
        let edf = get_rendered_edf("table-anno.toml").unwrap();
        assert!(edf.image == "ubuntu:anno");
        assert!(edf.annotations.get("two_plus_two").unwrap() == "four");
        assert!(edf.annotations.get("minus_one").unwrap() == "three");
        assert!(edf.annotations.get("quick").unwrap() == "maths");
    }

    #[test]
    #[serial]
    fn render_table_env() {
        let edf = get_rendered_edf("table-env.toml").unwrap();
        assert!(edf.image == "ubuntu:env");
        assert!(edf.env.get("two_plus_two").unwrap() == "four");
        assert!(edf.env.get("minus_one").unwrap() == "three");
        assert!(edf.env.get("quick").unwrap() == "maths");
    }

    #[test]
    #[serial]
    fn render_base_single() {
        let edf = get_rendered_edf("base-single.toml").unwrap();
        assert!(edf.image == "ubuntu:anno");
        assert!(edf.annotations.get("two_plus_two").unwrap() == "four");
        assert!(edf.annotations.get("minus_one").unwrap() == "three");
        assert!(edf.annotations.get("quick").unwrap() == "algebra");
    }

    #[test]
    #[serial]
    fn render_base_multi_1() {
        let edf = get_rendered_edf("base-multi-1.toml").unwrap();
        assert!(edf.image == "ubuntu:simple-1");
        assert!(edf.annotations.get("two_plus_two").unwrap() == "four");
        assert!(edf.annotations.get("minus_one").unwrap() == "three");
        assert!(edf.annotations.get("quick").unwrap() == "algebra");
    }

    #[test]
    #[serial]
    fn render_base_multi_2() {
        let edf = get_rendered_edf("base-multi-2.toml").unwrap();
        assert!(edf.image == "ubuntu:multi-2");
        assert!(edf.annotations.get("two_plus_two").unwrap() == "four");
        assert!(edf.annotations.get("minus_one").unwrap() == "three");
        assert!(edf.annotations.get("quick").unwrap() == "algebra");
        assert!(edf.env.get("two_plus_two").unwrap() == "four");
        assert!(edf.env.get("minus_one").unwrap() == "three");
        assert!(edf.env.get("quick").unwrap() == "counting");
    }

    #[test]
    #[serial]
    fn render_base_multi_vecs() {
        let edf = get_rendered_edf("base-multi-vecs.toml").unwrap();
        assert!(edf.image == "ubuntu:vecs");
        assert!(edf.devices.contains(&"dev1".to_string()));
        assert!(edf.devices.contains(&"dev2".to_string()));
        assert!(edf.devices.contains(&"dev3".to_string()));
        assert!(edf.devices.contains(&"dev4".to_string()));
        assert!(edf.devices.contains(&"dev5".to_string()));
        assert!(edf.devices.len() == 5);
        assert!(
            edf.mounts
                .iter()
                .any(|e| e.to_volume_string() == "/aaa:/bbb")
        );
        assert!(
            edf.mounts
                .iter()
                .any(|e| e.to_volume_string() == "./ccc:./ddd")
        );
        assert!(
            edf.mounts
                .iter()
                .any(|e| e.to_volume_string() == "/eee:./fff:ggg")
        );
        assert!(
            edf.mounts
                .iter()
                .any(|e| e.to_volume_string() == "/hhh:/iii")
        );
        assert!(
            edf.mounts
                .iter()
                .any(|e| e.to_volume_string() == "./jjj:./kkk")
        );
        assert!(edf.mounts.len() == 5);
    }

    #[test]
    #[serial]
    fn render_base_rec() {
        assert!(get_rendered_edf("base-rec.toml").is_err());
    }

    #[test]
    #[serial]
    fn render_base_nested() {
        let edf = get_rendered_edf("base-nested.toml").unwrap();
        assert!(edf.image == "ubuntu:anno");
        assert!(edf.annotations.get("two_plus_two").unwrap() == "four");
        assert!(edf.annotations.get("minus_one").unwrap() == "hot");
        assert!(edf.annotations.get("quick").unwrap() == "algebra");
    }

    #[test]
    #[serial]
    fn render_base_prio() {
        let edf = get_rendered_edf("base-prio.toml").unwrap();
        assert!(edf.image == "ubuntu:simple-1");
        assert!(edf.entrypoint == true);
    }

    #[test]
    #[serial]
    fn render_file_not_found() {
        let result = render(String::from("test/toml/not_found.toml"));
        assert!(result.is_err());
    }

    #[test]
    #[serial]
    fn render_not_a_toml_file() {
        let result = render(String::from("test/etc/plain.txt"));
        assert!(result.is_err());
    }

    #[test]
    #[serial]
    fn render_unknown_entry() {
        let result = render(String::from("test/toml/unknown_entry.toml"));
        assert!(result.is_ok());
    }
}
