use std::collections::BTreeMap;

use anyhow::{Result, bail};

pub type Vars = BTreeMap<&'static str, String>;

pub fn expand(template: &str, vars: &Vars) -> Result<String> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(i) = rest.find(['{', '}']) {
        out.push_str(&rest[..i]);
        let (c, after) = (rest.as_bytes()[i], &rest[i + 1..]);
        if after.starts_with(c as char) {
            // graffa raddoppiata: carattere letterale
            out.push(c as char);
            rest = &after[1..];
        } else if c == b'}' {
            bail!("unmatched '}}' in \"{template}\"");
        } else {
            let Some(end) = after.find('}') else {
                bail!("unclosed '{{' in \"{template}\"");
            };
            let name = &after[..end];
            match vars.get(name) {
                Some(v) => out.push_str(v),
                None => bail!(
                    "unknown placeholder {{{name}}} in \"{template}\" (known: {})",
                    vars.keys().copied().collect::<Vec<_>>().join(", ")
                ),
            }
            rest = &after[end + 1..];
        }
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> Vars {
        Vars::from([
            ("cc", "/llvm/clang".into()),
            ("copt", "-mllvm -x86-shadow-stack-pass".into()),
        ])
    }

    #[test]
    fn substitution() {
        assert_eq!(expand("CC={cc}", &vars()).unwrap(), "CC=/llvm/clang");
        assert_eq!(
            expand("{copt}", &vars()).unwrap(),
            "-mllvm -x86-shadow-stack-pass"
        );
        assert_eq!(expand("plain", &vars()).unwrap(), "plain");
        assert_eq!(expand("{{literal}}", &vars()).unwrap(), "{literal}");
    }

    #[test]
    fn errors() {
        assert!(expand("{nope}", &vars()).is_err());
        assert!(expand("{cc", &vars()).is_err());
        assert!(expand("cc}", &vars()).is_err());
    }
}
