//! Pi-style API-key credential storage backed by a user-level `auth.json`.

use std::{
    collections::BTreeMap,
    env, fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Credential {
    ApiKey { key: String },
}

#[derive(Debug)]
pub struct AuthStore {
    path: PathBuf,
    credentials: BTreeMap<String, Credential>,
}

impl AuthStore {
    pub fn load(path: PathBuf) -> Result<Self> {
        let credentials = match fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content)
                .with_context(|| format!("无法解析认证文件 {}", path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => {
                return Err(error).with_context(|| format!("无法读取认证文件 {}", path.display()));
            }
        };
        Ok(Self { path, credentials })
    }

    pub fn api_key(&self, provider: &str) -> Option<String> {
        match self.credentials.get(provider) {
            Some(Credential::ApiKey { key }) if !key.trim().is_empty() => Some(key.clone()),
            _ => None,
        }
    }

    pub fn providers(&self) -> Vec<String> {
        self.credentials.keys().cloned().collect()
    }

    pub fn import_api_keys(
        &mut self,
        entries: impl IntoIterator<Item = (String, String)>,
    ) -> Result<usize> {
        let mut imported = 0;
        for (provider, key) in entries {
            if key.trim().is_empty() || key.starts_with("your-") {
                continue;
            }
            self.credentials
                .insert(provider, Credential::ApiKey { key });
            imported += 1;
        }
        if imported > 0 {
            self.save()?;
        }
        Ok(imported)
    }

    pub fn set_api_key(&mut self, provider: String, key: String) -> Result<()> {
        let imported = self.import_api_keys([(provider, key)])?;
        if imported == 0 {
            anyhow::bail!("API Key 不能为空或仍是占位符");
        }
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn save(&self) -> Result<()> {
        let parent = self.path.parent().context("auth.json 路径缺少父目录")?;
        let parent_existed = parent.exists();
        fs::create_dir_all(parent)
            .with_context(|| format!("无法创建认证目录 {}", parent.display()))?;
        #[cfg(unix)]
        if !parent_existed {
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
                .with_context(|| format!("无法设置认证目录权限 {}", parent.display()))?;
        }

        let file_name = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("auth.json");
        let temp_path = parent.join(format!(".{file_name}.{}.tmp", std::process::id()));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        let mode = fs::metadata(&self.path)
            .map(|metadata| metadata.permissions().mode() & 0o777)
            .unwrap_or(0o600);
        #[cfg(unix)]
        options.mode(mode);

        let write_result = (|| -> Result<()> {
            let mut file = options
                .open(&temp_path)
                .with_context(|| format!("无法创建临时认证文件 {}", temp_path.display()))?;
            #[cfg(unix)]
            fs::set_permissions(&temp_path, fs::Permissions::from_mode(mode))?;
            let mut json = serde_json::to_string_pretty(&self.credentials)?;
            json.push('\n');
            file.write_all(json.as_bytes())?;
            file.sync_all()?;
            fs::rename(&temp_path, &self.path)
                .with_context(|| format!("无法更新认证文件 {}", self.path.display()))?;
            Ok(())
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }
        write_result
    }
}

pub fn auth_path() -> Result<PathBuf> {
    if let Some(path) = env::var_os("RICO_AUTH") {
        return Ok(PathBuf::from(path));
    }
    let home = env::var_os("HOME").context("无法确定用户目录：缺少 HOME")?;
    Ok(PathBuf::from(home).join(".config/rico/auth.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_auth_path() -> PathBuf {
        env::temp_dir().join(format!(
            "rico-auth-{}-{}.json",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn imports_and_reads_pi_style_api_keys() {
        let path = temp_auth_path();
        let mut store = AuthStore::load(path.clone()).unwrap();
        store
            .import_api_keys([
                ("minimax".into(), "minimax-secret".into()),
                ("9router".into(), "router-secret".into()),
            ])
            .unwrap();

        let loaded = AuthStore::load(path.clone()).unwrap();
        assert_eq!(loaded.api_key("minimax").as_deref(), Some("minimax-secret"));
        assert_eq!(loaded.api_key("9router").as_deref(), Some("router-secret"));
        assert_eq!(loaded.providers(), vec!["9router", "minimax"]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_invalid_auth_json() {
        let path = temp_auth_path();
        fs::write(&path, "[]").unwrap();
        assert!(AuthStore::load(path.clone()).is_err());
        let _ = fs::remove_file(path);
    }

    #[cfg(unix)]
    #[test]
    fn creates_auth_file_with_owner_only_permissions() {
        let path = temp_auth_path();
        let mut store = AuthStore::load(path.clone()).unwrap();
        store
            .import_api_keys([("minimax".into(), "secret".into())])
            .unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let _ = fs::remove_file(path);
    }
}
