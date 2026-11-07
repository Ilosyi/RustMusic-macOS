//! 平台机密存储的统一入口（服务名 + 账号 → 机密字符串）。
//!
//! Windows / Linux：系统凭据管理器（keyring：Windows 凭据管理器 / Secret Service）。
//! macOS：Application Support 目录下的 0600 JSON 文件。
//!
//! macOS 为什么不走钥匙串：Security.framework 把条目的 ACL 绑定到写入者的代码
//! 签名（cdhash）。本项目只能做 ad-hoc 签名（开源分发无 Developer ID），每次
//! 重新构建或升级 cdhash 都会变，于是「读自己写的条目」也会弹出
//! 「RustMusic 想要使用你储存在钥匙串中的机密信息…请输入密码」，该弹窗同步
//! 阻塞调用线程，应用直接卡在启动画面，用户除了手动输密码无路可走。
//! 改用文件存储后：权限 0600、只有当前用户能读，升级/重装不再要求重新登录，
//! 行为与 Windows 凭据管理器一致（同为「本机当前用户」级别的隔离）。

use std::path::PathBuf;
use std::sync::OnceLock;

static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

/// 由启动流程注入应用数据目录（与 library.db 同目录）
pub fn init(app_data_dir: PathBuf) {
    let _ = DATA_DIR.set(app_data_dir);
}

#[cfg(target_os = "macos")]
fn data_dir() -> PathBuf {
    if let Some(d) = DATA_DIR.get() {
        return d.clone();
    }
    // 兜底：注入之前访问（命令行工具/单测）按 macOS 约定推导
    std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join("Library/Application Support/com.rustmusic.app"))
        .unwrap_or_else(std::env::temp_dir)
}

/// 读取机密；不存在或为空返回 None
pub fn get(service: &str, account: &str) -> Option<String> {
    imp::get(service, account)
}

/// 写入机密（空串 = 清除）
pub fn set(service: &str, account: &str, value: &str) -> Result<(), String> {
    imp::set(service, account, value)
}

/// 删除机密（幂等）
pub fn delete(service: &str, account: &str) -> Result<(), String> {
    imp::set(service, account, "")
}

#[cfg(target_os = "macos")]
mod imp {
    use super::data_dir;
    use std::collections::BTreeMap;
    use std::fs;
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    fn path() -> std::path::PathBuf {
        data_dir().join("credentials.json")
    }

    /// 键 = 服务名 + 单元分隔符 + 账号（两个字段都可能含任意字符，用 US 分隔）
    fn key(service: &str, account: &str) -> String {
        format!("{service}\u{1f}{account}")
    }

    fn load() -> BTreeMap<String, String> {
        match fs::read_to_string(path()) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => BTreeMap::new(),
        }
    }

    fn save(map: &BTreeMap<String, String>) -> Result<(), String> {
        let p = path();
        if let Some(dir) = p.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("凭据目录创建失败: {e}"))?;
        }
        let body = serde_json::to_vec_pretty(map).map_err(|e| format!("凭据序列化失败: {e}"))?;
        let tmp = p.with_extension("json.tmp");
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|e| format!("凭据文件写入失败: {e}"))?;
        f.write_all(&body)
            .map_err(|e| format!("凭据文件写入失败: {e}"))?;
        f.sync_all().ok();
        drop(f);
        // 已存在的临时文件不受 mode() 约束，显式收紧权限后再原子替换
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
        fs::rename(&tmp, &p).map_err(|e| format!("凭据文件替换失败: {e}"))?;
        Ok(())
    }

    pub fn get(service: &str, account: &str) -> Option<String> {
        load().remove(&key(service, account)).filter(|s| !s.is_empty())
    }

    pub fn set(service: &str, account: &str, value: &str) -> Result<(), String> {
        let mut m = load();
        let k = key(service, account);
        if value.is_empty() {
            // 清除不存在的条目同样视为成功（幂等）
            m.remove(&k);
        } else {
            m.insert(k, value.to_string());
        }
        save(&m)
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    pub fn get(service: &str, account: &str) -> Option<String> {
        let e = keyring::Entry::new(service, account).ok()?;
        e.get_password().ok().filter(|s| !s.is_empty())
    }

    pub fn set(service: &str, account: &str, value: &str) -> Result<(), String> {
        let e = keyring::Entry::new(service, account)
            .map_err(|e| format!("凭据管理器不可用: {e}"))?;
        if value.is_empty() {
            match e.delete_credential() {
                Ok(()) => Ok(()),
                Err(keyring::Error::NoEntry) => Ok(()),
                Err(err) => Err(format!("清除凭据失败: {err}")),
            }
        } else {
            e.set_password(value)
                .map_err(|err| format!("保存凭据失败: {err}"))
        }
    }
}
