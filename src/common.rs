use regex::Regex;
use std::collections::HashMap;
use std::process::Command;

use crate::error::{SarusError, SarusResult};

pub fn expand_vars_string(
    input: String,
    env: &Option<HashMap<String, String>>,
) -> SarusResult<String> {
    match env {
        Some(h) => expand_vars_string_with_env(input, &h),
        None => expand_vars_string_without_env(input),
    }
}

fn expand_vars_string_with_env(
    input: String,
    env: &HashMap<String, String>,
) -> SarusResult<String> {
    // Ban any strings that will attempt to execute something upon evaluation.
    let re_banned = Regex::new(r#"([^\\]|^)(\$\(|`|;|")"#).unwrap();
    if re_banned.is_match(&input) {
        return Err(SarusError {
            code: 18,
            file_path: None,
            msg: String::from(format!("cannot expand string {input}, invalid string")),
        });
    }

    // Evaluate 'input' in a restricted shell.
    // This will block any redirection attempts, among other things.
    let output = Command::new("bash")
        .arg("-r")
        .arg("-c")
        .arg(format!("set -u; echo -n \"{}\"", &input))
        .env_clear()
        .envs(env)
        .output();

    let stdout = match output {
        Ok(o) => o.stdout,
        Err(e) => {
            return Err(SarusError {
                code: 18,
                file_path: None,
                msg: String::from(format!("cannot expand string {input}, {e}")),
            });
        }
    };

    let out = match str::from_utf8(&stdout) {
        Ok(o) => o,
        Err(e) => {
            return Err(SarusError {
                code: 19,
                file_path: None,
                msg: String::from(format!("cannot expand string {input}, {e}")),
            });
        }
    };

    Ok(String::from(out))
}

fn expand_vars_string_without_env(s: String) -> SarusResult<String> {
    match shellexpand::env(&s) {
        Ok(ok) => return Ok(ok.to_string()),
        Err(e) => {
            return Err(SarusError {
                code: 17,
                file_path: None,
                msg: String::from(format!(
                    "cannot expand variable {}, {}",
                    e.var_name, e.cause
                )),
            });
        }
    };
}

pub fn expand_vars_hashmap(
    h: HashMap<String, String>,
    env: &Option<HashMap<String, String>>,
) -> SarusResult<HashMap<String, String>> {
    let mut newh = h.clone();
    for (k, v) in h {
        let ev = expand_vars_string(v.clone(), env)?;
        if ev != v {
            newh.insert(k, ev);
        }
    }
    return Ok(newh);
}

pub fn expand_vars_vec(
    v: Vec<String>,
    env: &Option<HashMap<String, String>>,
) -> SarusResult<Vec<String>> {
    let mut newv = vec![];
    for s in v {
        newv.push(expand_vars_string(s, env)?);
    }
    return Ok(newv);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check_expand_vars_string(input: &str, expected: &str) -> bool {
        let mut env = HashMap::new();
        env.insert("XXX".to_string(), "111".to_string());
        match expand_vars_string_with_env(input.to_string(), &env) {
            Ok(s) => {
                println!("{}", s);
                return s == expected;
            }
            Err(_) => return false,
        }
    }

    #[test]
    fn expand_vars_normal_strs() {
        assert!(check_expand_vars_string(
            r#"xxx-$XXX-xxx"#,
            r#"xxx-111-xxx"#
        ));
        assert!(check_expand_vars_string(
            r#"xxx-${XXX}-xxx"#,
            r#"xxx-111-xxx"#
        ));
        assert!(check_expand_vars_string(
            r#"xxx-${XXX:1}-xxx"#,
            r#"xxx-11-xxx"#
        ));
        assert!(check_expand_vars_string(
            r#"xxx-\$(XXX)-xxx"#,
            r#"xxx-$(XXX)-xxx"#
        ));
        assert!(check_expand_vars_string(
            r#"xxx-$\(XXX)-xxx"#,
            r#"xxx-$\(XXX)-xxx"#
        ));
        assert!(check_expand_vars_string(
            r#"xxx-\`XXX\`-xxx"#,
            r#"xxx-`XXX`-xxx"#
        ));
        assert!(check_expand_vars_string(r#"xxx--xxx\;"#, r#"xxx--xxx\;"#));
        assert!(check_expand_vars_string(r#"\"xxx--xxx\""#, r#""xxx--xxx""#));
    }

    #[test]
    fn expand_vars_unknown_var() {
        assert!(!check_expand_vars_string(
            r#"xxx-${YYY}-xxx"#,
            r#"xxx--xxx"#
        ));
        assert!(check_expand_vars_string(
            r#"xxx-${YYY:-222}-xxx"#,
            r#"xxx-222-xxx"#
        ));
    }

    #[test]
    fn expand_vars_banned_strs() {
        assert!(!check_expand_vars_string(r#"xxx-$(XXX)-xxx"#, ""));
        assert!(!check_expand_vars_string(r#"xxx-`XXX`-xxx"#, ""));
        assert!(!check_expand_vars_string(r#"xxx-"-xxx"#, ""));
        assert!(!check_expand_vars_string(r#"xxx--xxx;"#, ""));
        assert!(!check_expand_vars_string(r#"$(XXX)xxx--xxx"#, ""));
        assert!(!check_expand_vars_string(r#"`XXX`xxx--xxx"#, ""));
        assert!(!check_expand_vars_string(r#""xxx--xxx"#, ""));
        assert!(!check_expand_vars_string(r#"; xxx--xxx"#, ""));
        assert!(!check_expand_vars_string(r#"" >/tmp/file"#, ""));
    }
}
