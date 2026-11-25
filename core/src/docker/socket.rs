//! Which Unix socket the docker CLI would talk to.
//!
//! Same order as the CLI: `DOCKER_HOST`, then `DOCKER_CONTEXT`, then
//! `currentContext` in the CLI config. A named context's endpoint lives in
//! `<config dir>/contexts/meta/<sha256 hex of the name>/meta.json`. Only
//! `unix://` endpoints are supported (tcp, ssh, npipe → None).

use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const VAR_RUN_SOCK: &str = "/var/run/docker.sock";

/// `env` reads an environment variable (`|k| std::env::var(k).ok()` in the
/// app), `home` is the user's home directory.
pub fn resolve_socket(env: impl Fn(&str) -> Option<String>, home: &Path) -> Option<PathBuf> {
    resolve_in(env, home, Path::new(VAR_RUN_SOCK))
}

pub(crate) fn resolve_in(env: impl Fn(&str) -> Option<String>, home: &Path, var_run: &Path) -> Option<PathBuf> {
    let nonempty = |k: &str| env(k).filter(|v| !v.trim().is_empty());

    if let Some(host) = nonempty("DOCKER_HOST") {
        return unix_path(&host);
    }

    // The CLI honours DOCKER_CONFIG for the whole config directory.
    let config_dir = match nonempty("DOCKER_CONFIG") {
        Some(dir) => PathBuf::from(dir),
        None => home.join(".docker"),
    };
    let context = nonempty("DOCKER_CONTEXT").or_else(|| current_context(&config_dir));
    match context.as_deref() {
        None | Some("default") => Some(default_socket(home, var_run)),
        Some(name) => context_endpoint(&config_dir, name),
    }
}

/// `/var/run/docker.sock`, or Docker Desktop's per-user socket when the
/// `/var/run` symlink was not installed. When neither exists yet (Docker not
/// started), the standard path, so a later start is picked up.
fn default_socket(home: &Path, var_run: &Path) -> PathBuf {
    if var_run.exists() {
        return var_run.to_path_buf();
    }
    let desktop = home.join(".docker/run/docker.sock");
    if desktop.exists() { desktop } else { var_run.to_path_buf() }
}

fn unix_path(host: &str) -> Option<PathBuf> {
    let path = host.trim().strip_prefix("unix://")?;
    (!path.is_empty()).then(|| PathBuf::from(path))
}

fn current_context(config_dir: &Path) -> Option<String> {
    #[derive(Deserialize)]
    struct Config {
        #[serde(rename = "currentContext")]
        current_context: Option<String>,
    }
    let bytes = std::fs::read(config_dir.join("config.json")).ok()?;
    let cfg: Config = serde_json::from_slice(&bytes).ok()?;
    cfg.current_context.filter(|c| !c.is_empty())
}

fn context_endpoint(config_dir: &Path, name: &str) -> Option<PathBuf> {
    #[derive(Deserialize)]
    struct Meta {
        #[serde(rename = "Endpoints")]
        endpoints: Endpoints,
    }
    #[derive(Deserialize)]
    struct Endpoints {
        docker: Endpoint,
    }
    #[derive(Deserialize)]
    struct Endpoint {
        #[serde(rename = "Host")]
        host: String,
    }
    let digest = Sha256::digest(name.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    let bytes = std::fs::read(config_dir.join("contexts/meta").join(hex).join("meta.json")).ok()?;
    let meta: Meta = serde_json::from_slice(&bytes).ok()?;
    unix_path(&meta.endpoints.docker.host)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::fs;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("devload-sock-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            Tmp(p)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |k| m.get(k).cloned()
    }

    fn write_context(config_dir: &Path, name: &str, host: &str) {
        let hex: String = Sha256::digest(name.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
        let dir = config_dir.join("contexts/meta").join(hex);
        fs::create_dir_all(&dir).unwrap();
        let meta = format!(
            r#"{{"Name":"{name}","Metadata":{{"Description":"x"}},"Endpoints":{{"docker":{{"Host":"{host}","SkipTLSVerify":false}}}}}}"#
        );
        fs::write(dir.join("meta.json"), meta).unwrap();
    }

    #[test]
    fn context_dir_name_is_sha256_of_the_name() {
        let hex: String = Sha256::digest(b"desktop-linux").iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "fe9c6bd7a66301f49ca9b6a70b217107cd1284598bfc254700c989b916da791e");
    }

    #[test]
    fn docker_host_wins_and_only_unix_is_supported() {
        let t = Tmp::new("host");
        let vr = t.0.join("none.sock");
        let r = resolve_in(env(&[("DOCKER_HOST", "unix:///tmp/x.sock"), ("DOCKER_CONTEXT", "c")]), &t.0, &vr);
        assert_eq!(r, Some(PathBuf::from("/tmp/x.sock")));
        assert_eq!(resolve_in(env(&[("DOCKER_HOST", "tcp://127.0.0.1:2375")]), &t.0, &vr), None);
        assert_eq!(resolve_in(env(&[("DOCKER_HOST", "ssh://me@box")]), &t.0, &vr), None);
        assert_eq!(resolve_in(env(&[("DOCKER_HOST", "unix://")]), &t.0, &vr), None);
        // Empty DOCKER_HOST is ignored, like the CLI does.
        assert_eq!(resolve_in(env(&[("DOCKER_HOST", "")]), &t.0, &vr), Some(vr.clone()));
    }

    #[test]
    fn current_context_from_config() {
        let t = Tmp::new("cfg");
        let home = &t.0;
        fs::create_dir_all(home.join(".docker")).unwrap();
        fs::write(home.join(".docker/config.json"), r#"{"auths":{},"currentContext":"desktop-linux"}"#).unwrap();
        write_context(&home.join(".docker"), "desktop-linux", "unix:///Users/dev/.docker/run/docker.sock");
        let vr = home.join("none.sock");
        assert_eq!(
            resolve_in(env(&[]), home, &vr),
            Some(PathBuf::from("/Users/dev/.docker/run/docker.sock"))
        );

        // DOCKER_CONTEXT beats the config file.
        write_context(&home.join(".docker"), "colima", "unix:///Users/dev/.colima/default/docker.sock");
        assert_eq!(
            resolve_in(env(&[("DOCKER_CONTEXT", "colima")]), home, &vr),
            Some(PathBuf::from("/Users/dev/.colima/default/docker.sock"))
        );
        // "default" means the standard socket.
        assert_eq!(resolve_in(env(&[("DOCKER_CONTEXT", "default")]), home, &vr), Some(vr.clone()));
        // A context with a tcp endpoint, or one that does not exist.
        write_context(&home.join(".docker"), "remote", "tcp://10.0.0.2:2376");
        assert_eq!(resolve_in(env(&[("DOCKER_CONTEXT", "remote")]), home, &vr), None);
        assert_eq!(resolve_in(env(&[("DOCKER_CONTEXT", "missing")]), home, &vr), None);
    }

    #[test]
    fn docker_config_env_moves_the_config_dir() {
        let t = Tmp::new("dcfg");
        let cfg = t.0.join("elsewhere");
        fs::create_dir_all(&cfg).unwrap();
        fs::write(cfg.join("config.json"), r#"{"currentContext":"orbstack"}"#).unwrap();
        write_context(&cfg, "orbstack", "unix:///Users/dev/.orbstack/run/docker.sock");
        let r = resolve_in(env(&[("DOCKER_CONFIG", cfg.to_str().unwrap())]), &t.0, &t.0.join("none.sock"));
        assert_eq!(r, Some(PathBuf::from("/Users/dev/.orbstack/run/docker.sock")));
    }

    #[test]
    fn default_socket_fallbacks() {
        let t = Tmp::new("def");
        let home = &t.0;
        let vr = home.join("var-run.sock");
        // Nothing exists: the standard path.
        assert_eq!(resolve_in(env(&[]), home, &vr), Some(vr.clone()));
        // Broken config.json: treated as no context.
        fs::create_dir_all(home.join(".docker/run")).unwrap();
        fs::write(home.join(".docker/config.json"), "{not json").unwrap();
        // Only Docker Desktop's per-user socket exists.
        fs::write(home.join(".docker/run/docker.sock"), "").unwrap();
        assert_eq!(resolve_in(env(&[]), home, &vr), Some(home.join(".docker/run/docker.sock")));
        // /var/run/docker.sock exists: it wins.
        fs::write(&vr, "").unwrap();
        assert_eq!(resolve_in(env(&[]), home, &vr), Some(vr.clone()));
    }
}
