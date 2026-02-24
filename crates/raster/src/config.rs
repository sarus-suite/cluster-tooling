use crate::common::expand_vars_string;
use crate::{EDF, SarusError, SarusResult, check_file_path_extension, validate_file};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const CONFIG_PATH: &str = "/etc/sarus-suite";

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct RawConfig {
    edf_system_search_path: Option<String>,
    parallax_imagestore: Option<String>,
    parallax_mount_program: Option<String>,
    parallax_path: Option<String>,
    parallax_mp_squashfuse_cmd: Option<String>,
    perfmon: Option<bool>,
    podman_module: Option<String>,
    podman_path: Option<String>,
    podman_tmp_path: Option<String>,
    runtime_path: Option<String>,
    skybox_enabled: Option<bool>,
    tracking_enabled: Option<bool>,
    tracking_tool: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Config {
    #[serde(default = "get_default_edf_system_search_path")]
    pub edf_system_search_path: String,
    #[serde(default = "get_default_parallax_imagestore")]
    pub parallax_imagestore: String,
    #[serde(default = "get_default_parallax_mount_program")]
    pub parallax_mount_program: String,
    #[serde(default = "get_default_parallax_path")]
    pub parallax_path: String,
    #[serde(default = "get_default_parallax_mp_squashfuse_cmd")]
    pub parallax_mp_squashfuse_cmd: String,
    #[serde(default = "get_default_perfmon")]
    pub perfmon: bool,
    #[serde(default = "get_default_podman_module")]
    pub podman_module: String,
    #[serde(default = "get_default_podman_path")]
    pub podman_path: String,
    #[serde(default = "get_default_podman_tmp_path")]
    pub podman_tmp_path: String,
    #[serde(default = "get_default_runtime_path")]
    pub runtime_path: String,
    #[serde(default = "get_default_skybox_enabled")]
    pub skybox_enabled: bool,
    #[serde(default = "get_default_tracking_enabled")]
    pub tracking_enabled: bool,
    #[serde(default = "get_default_tracking_tool")]
    pub tracking_tool: String,
}

#[derive(Clone, Copy)]
pub enum VarExpand {
    Never, // Do not expand variables.
    Try,   // Try to expand variables, return original string in case of errors.
    Must,  // Expand variables, return Error in case of errors.
}

fn get_default_edf_system_search_path() -> String {
    return String::from("/etc/edf");
}

fn get_default_parallax_imagestore() -> String {
    return String::from("");
}

fn get_default_parallax_mount_program() -> String {
    return String::from("");
}

fn get_default_parallax_path() -> String {
    return String::from("parallax");
}

fn get_default_parallax_mp_squashfuse_cmd() -> String {
    return String::from("squashfuse_ll");
}

fn get_default_perfmon() -> bool {
    return false;
}

fn get_default_podman_module() -> String {
    return String::from("hpc");
}

fn get_default_podman_path() -> String {
    return String::from("podman");
}

fn get_default_podman_tmp_path() -> String {
    return String::from("/dev/shm");
}

fn get_default_runtime_path() -> String {
    return String::from("crun");
}

fn get_default_skybox_enabled() -> bool {
    return false;
}

fn get_default_tracking_enabled() -> bool {
    return false;
}

fn get_default_tracking_tool() -> String {
    return String::from("");
}

impl From<RawConfig> for Config {
    fn from(r: RawConfig) -> Self {
        Config {
            edf_system_search_path: match r.edf_system_search_path {
                Some(s) => s,
                None => get_default_edf_system_search_path(),
            },
            parallax_imagestore: match r.parallax_imagestore {
                Some(s) => s,
                None => get_default_parallax_imagestore(),
            },
            parallax_mount_program: match r.parallax_mount_program {
                Some(s) => s,
                None => get_default_parallax_mount_program(),
            },
            parallax_path: match r.parallax_path {
                Some(s) => s,
                None => get_default_parallax_path(),
            },
            parallax_mp_squashfuse_cmd: match r.parallax_mp_squashfuse_cmd {
                Some(s) => s,
                None => get_default_parallax_mp_squashfuse_cmd(),
            },
            perfmon: match r.perfmon {
                Some(s) => s,
                None => get_default_perfmon(),
            },
            podman_module: match r.podman_module {
                Some(s) => s,
                None => get_default_podman_module(),
            },
            podman_path: match r.podman_path {
                Some(s) => s,
                None => get_default_podman_path(),
            },
            podman_tmp_path: match r.podman_tmp_path {
                Some(s) => s,
                None => get_default_podman_tmp_path(),
            },
            runtime_path: match r.runtime_path {
                Some(s) => s,
                None => get_default_runtime_path(),
            },
            skybox_enabled: match r.skybox_enabled {
                Some(s) => s,
                None => get_default_skybox_enabled(),
            },
            tracking_enabled: match r.tracking_enabled {
                Some(s) => s,
                None => get_default_tracking_enabled(),
            },
            tracking_tool: match r.tracking_tool {
                Some(s) => s,
                None => get_default_tracking_tool(),
            },
        }
    }
}

impl RawConfig {
    // Overwrite values with the other RawConfig
    fn extend(&mut self, i: RawConfig) {
        if i.edf_system_search_path.is_some() {
            self.edf_system_search_path = i.edf_system_search_path;
        }
        if i.parallax_imagestore.is_some() {
            self.parallax_imagestore = i.parallax_imagestore;
        }
        if i.parallax_mount_program.is_some() {
            self.parallax_mount_program = i.parallax_mount_program;
        }
        if i.parallax_path.is_some() {
            self.parallax_path = i.parallax_path;
        }
        if i.parallax_mp_squashfuse_cmd.is_some() {
            self.parallax_mp_squashfuse_cmd = i.parallax_mp_squashfuse_cmd;
        }
        if i.perfmon.is_some() {
            self.perfmon = i.perfmon;
        }
        if i.podman_module.is_some() {
            self.podman_module = i.podman_module;
        }
        if i.podman_path.is_some() {
            self.podman_path = i.podman_path;
        }
        if i.podman_tmp_path.is_some() {
            self.podman_tmp_path = i.podman_tmp_path;
        }
        if i.runtime_path.is_some() {
            self.runtime_path = i.runtime_path;
        }
        if i.skybox_enabled.is_some() {
            self.skybox_enabled = i.skybox_enabled;
        }
        if i.tracking_enabled.is_some() {
            self.tracking_enabled = i.tracking_enabled;
        }
        if i.tracking_tool.is_some() {
            self.tracking_tool = i.tracking_tool;
        }
    }
}

fn validate_configfile(path: String) -> SarusResult<()> {
    // Embedding schema file
    let schema_content = include_str!("schema/config.json");

    check_file_path_extension(&path, "conf")?;

    validate_file(path, schema_content)
}

fn load_raw_config_from_file(
    filepath: String,
    force_expand: VarExpand,
    env_option: &Option<HashMap<String, String>>,
) -> SarusResult<RawConfig> {
    //let empty = RawConfig::default();

    validate_configfile(filepath.clone())?;

    let path_str = filepath.as_str();

    //To be replaced by toml_read when it will work.
    let toml_content = match std::fs::read_to_string(path_str) {
        Ok(c) => c,
        Err(e) => {
            return Err(SarusError {
                code: 2,
                file_path: Some(String::from(path_str)),
                msg: String::from(format!("{}", e)),
            });
        }
    };

    let toml_value = match toml::from_str(&toml_content) {
        Ok(v) => v,
        Err(e) => {
            return Err(SarusError {
                code: 3,
                file_path: Some(String::from(path_str)),
                msg: String::from(format!("{}", e)),
            });
        }
    };

    let mut r: RawConfig = toml_value;
    //let mut r: RawConfig = toml_read(path_str)?;

    // Expand variables in the fields
    expand_raw_config_fields(&mut r, force_expand, env_option)?;

    Ok(r)
}

fn expand_raw_config_fields(
    r: &mut RawConfig,
    force_expand: VarExpand,
    e: &Option<HashMap<String, String>>,
) -> SarusResult<()> {
    let force = match force_expand {
        VarExpand::Never => return Ok(()),
        VarExpand::Try => false,
        VarExpand::Must => true,
    };

    expand_raw_option_string(&mut r.edf_system_search_path, force, e)?;
    expand_raw_option_string(&mut r.parallax_imagestore, force, e)?;
    expand_raw_option_string(&mut r.parallax_mount_program, force, e)?;
    expand_raw_option_string(&mut r.parallax_path, force, e)?;
    expand_raw_option_string(&mut r.parallax_mp_squashfuse_cmd, force, e)?;
    expand_raw_option_string(&mut r.podman_module, force, e)?;
    expand_raw_option_string(&mut r.podman_path, force, e)?;
    expand_raw_option_string(&mut r.podman_tmp_path, force, e)?;
    expand_raw_option_string(&mut r.runtime_path, force, e)?;
    expand_raw_option_string(&mut r.tracking_tool, force, e)?;
    Ok(())
}

fn expand_raw_option_string(
    optstr: &mut Option<String>,
    force: bool,
    env_option: &Option<HashMap<String, String>>,
) -> SarusResult<()> {
    if optstr.is_some() {
        let original = optstr.clone().unwrap();
        let updated = match expand_vars_string(original.clone(), env_option) {
            Ok(s) => s,
            Err(e) => {
                if force {
                    return Err(e);
                } else {
                    original
                }
            }
        };
        *optstr = Some(updated);
        //*optstr = Some(expand_vars_string(original_string, env_option)?);
    }
    Ok(())
}

pub fn load_config() -> SarusResult<Config> {
    load_config_path(None, VarExpand::Must, &None)
}

pub fn load_config_path(
    config_option: Option<PathBuf>,
    force_expand: VarExpand,
    env_option: &Option<HashMap<String, String>>,
) -> SarusResult<Config> {
    let config_path = match config_option {
        Some(path) => path,
        None => PathBuf::from(CONFIG_PATH),
    };

    let r = load_raw_config_from_dir(&config_path, force_expand, env_option)?;
    let c = Config::from(r);
    Ok(c)
}

fn load_raw_config_from_dir(
    config_path: &Path,
    force_expand: VarExpand,
    env_option: &Option<HashMap<String, String>>,
) -> SarusResult<RawConfig> {
    let empty = RawConfig::default();

    let readdir = match std::fs::read_dir(config_path) {
        Ok(ok) => ok,
        Err(emsg) => {
            return Err(SarusError {
                code: 23,
                file_path: Some(config_path.to_string_lossy().to_string()),
                msg: String::from(format!("Cannot find config files, {}", emsg)),
            });
        }
    };

    let mut rcfg = empty;
    let mut entries = readdir
        .filter_map(Result::ok)
        .collect::<Vec<std::fs::DirEntry>>();

    entries.sort_by_key(|dir| dir.path());

    for e in entries {
        let file_name = match e.file_name().into_string() {
            Ok(s) => s,
            Err(_) => continue,
        };

        let file_path = match e.path().as_os_str().to_str() {
            Some(s) => s.to_string(),
            None => continue,
        };

        if e.path().is_dir() {
            continue;
        }

        if file_name.ends_with(".conf") {
            let cur_rcfg = load_raw_config_from_file(file_path, force_expand, env_option)?;
            rcfg.extend(cur_rcfg);
        }
    }
    Ok(rcfg)
}

pub fn update_config_by_user(config: &mut Config, edf: EDF) -> SarusResult<()> {
    let parallax_imagestore = edf.annotations.get("com.sarus.parallax_imagestore");
    if parallax_imagestore.is_some() {
        config.parallax_imagestore = parallax_imagestore.unwrap().to_string();
    }

    let parallax_mount_program = edf.annotations.get("com.sarus.parallax_mount_program");
    if parallax_mount_program.is_some() {
        config.parallax_mount_program = parallax_mount_program.unwrap().to_string();
    }

    let parallax_path = edf.annotations.get("com.sarus.parallax_path");
    if parallax_path.is_some() {
        config.parallax_path = parallax_path.unwrap().to_string();
    }

    let perfmon = edf.annotations.get("com.sarus.perfmon");
    if perfmon.is_some() {
        config.perfmon = match perfmon.unwrap().as_str() {
            "true" => true,
            "false" => false,
            _ => config.perfmon,
        };
    }

    let podman_module = edf.annotations.get("com.sarus.podman_module");
    if podman_module.is_some() {
        config.podman_module = podman_module.unwrap().to_string();
    }

    let podman_path = edf.annotations.get("com.sarus.podman_path");
    if podman_path.is_some() {
        config.podman_path = podman_path.unwrap().to_string();
    }

    let podman_tmp_path = edf.annotations.get("com.sarus.podman_tmp_path");
    if podman_tmp_path.is_some() {
        config.podman_tmp_path = podman_tmp_path.unwrap().to_string();
    }

    let runtime_path = edf.annotations.get("com.sarus.runtime_path");
    if runtime_path.is_some() {
        config.runtime_path = runtime_path.unwrap().to_string();
    }

    let skybox_enabled = edf.annotations.get("com.sarus.skybox_enabled");
    if skybox_enabled.is_some() {
        config.skybox_enabled = match skybox_enabled.unwrap().as_str() {
            "true" => true,
            "false" => false,
            _ => config.skybox_enabled,
        };
    }

    let tracking_enabled = edf.annotations.get("com.sarus.tracking_enabled");
    if tracking_enabled.is_some() {
        config.tracking_enabled = match tracking_enabled.unwrap().as_str() {
            "true" => true,
            "false" => false,
            _ => config.tracking_enabled,
        };
    }

    let tracking_tool = edf.annotations.get("com.sarus.tracking_tool");
    if tracking_tool.is_some() {
        config.tracking_tool = tracking_tool.unwrap().to_string();
    }
    Ok(())
}

pub fn remove_sarus_annotations(edf: &mut EDF) -> SarusResult<()> {
    let loop_edf = edf.clone();

    for key in loop_edf.annotations.keys() {
        if key.starts_with("com.sarus.") {
            edf.annotations.remove(key);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::get_rendered_edf;
    use serial_test::serial;

    fn get_rendered_config(cfg_dir: &str) -> SarusResult<Config> {
        let cwd = std::env::current_dir()
            .unwrap()
            .into_os_string()
            .into_string()
            .unwrap();
        let cfg_path = format!("{}/test/{}", cwd, cfg_dir);
        let opt_cfg_d = Some(PathBuf::from(cfg_path));
        load_config_path(opt_cfg_d, VarExpand::Must, &None)
    }

    #[test]
    fn load_config() {
        let cfg = get_rendered_config("config").unwrap();
        let pwd = std::env::var("PWD").unwrap();
        let expected_imagestore = format!("{pwd}/imagestore");

        assert!(cfg.edf_system_search_path == "/etc/edf_test");
        assert!(cfg.parallax_imagestore == expected_imagestore);
        assert!(cfg.parallax_mount_program == "parallax_mount_program77");
        assert!(cfg.parallax_path == "parallax50");
        assert!(cfg.perfmon == false);
        assert!(cfg.podman_module == "hpc");
        assert!(cfg.podman_path == "podman01");
        assert!(cfg.podman_tmp_path == "podman_tmp_path");
        assert!(cfg.runtime_path == "crun99");
        assert!(cfg.skybox_enabled == true);
        assert!(cfg.tracking_enabled == false);
        assert!(cfg.tracking_tool == "");
    }

    #[test]
    fn load_config_unquoted() {
        let result = get_rendered_config("config.unquoted");
        assert!(result.is_err())
    }

    #[test]
    #[serial]
    fn merge_config_and_edf() {
        let mut cfg = get_rendered_config("config").unwrap();
        let edf = get_rendered_edf("config_test.toml").unwrap();

        let pwd = std::env::var("PWD").unwrap();
        let expected_tracking_tool = format!("{pwd}/tracking_tool_edf");

        let _ = update_config_by_user(&mut cfg, edf);
        assert!(cfg.parallax_imagestore == "parallax_imagestore_edf");
        assert!(cfg.parallax_mount_program == "parallax_mount_program_edf");
        assert!(cfg.parallax_path == "parallax_path_edf");
        assert!(cfg.perfmon == true);
        assert!(cfg.podman_module == "hpc_edf");
        assert!(cfg.podman_path == "podman_path_edf");
        assert!(cfg.podman_tmp_path == "podman_tmp_path_edf");
        assert!(cfg.runtime_path == "crun_edf");
        assert!(cfg.skybox_enabled == false);
        assert!(cfg.tracking_enabled == false);
        assert!(cfg.tracking_tool == expected_tracking_tool);
    }
}
