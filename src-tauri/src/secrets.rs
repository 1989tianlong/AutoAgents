//! 凭据存储：GitHub PAT、中继令牌、与各节点配对的密钥、部署平台的令牌。
//!
//! 一律存系统 Keychain：不进 SQLite、不写日志、不返回给界面。测试里换成内存实现。

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::Mutex;

/// Keychain 里的服务名
pub const SERVICE: &str = "com.tianlong.autoagents";

pub trait SecretStore: Send + Sync {
    fn get(&self, account: &str) -> Result<Option<String>, String>;
    fn set(&self, account: &str, value: &str) -> Result<(), String>;
    fn delete(&self, account: &str) -> Result<(), String>;
}

/// 系统 Keychain
pub struct KeychainStore;

impl SecretStore for KeychainStore {
    fn get(&self, account: &str) -> Result<Option<String>, String> {
        let entry = keyring::Entry::new(SERVICE, account)
            .map_err(|e| format!("Keychain 初始化失败：{e}"))?;
        match entry.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(format!("读取 Keychain 失败：{e}")),
        }
    }

    fn set(&self, account: &str, value: &str) -> Result<(), String> {
        let entry = keyring::Entry::new(SERVICE, account)
            .map_err(|e| format!("Keychain 初始化失败：{e}"))?;
        entry
            .set_password(value)
            .map_err(|e| format!("Keychain 写入失败：{e}"))
    }

    fn delete(&self, account: &str) -> Result<(), String> {
        let entry = keyring::Entry::new(SERVICE, account)
            .map_err(|e| format!("Keychain 初始化失败：{e}"))?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(format!("Keychain 删除失败：{e}")),
        }
    }
}

/// 内存实现：测试用
#[cfg(test)]
#[derive(Default)]
pub struct MemoryStore(Mutex<HashMap<String, String>>);

#[cfg(test)]
impl SecretStore for MemoryStore {
    fn get(&self, account: &str) -> Result<Option<String>, String> {
        Ok(self.0.lock().unwrap().get(account).cloned())
    }

    fn set(&self, account: &str, value: &str) -> Result<(), String> {
        self.0
            .lock()
            .unwrap()
            .insert(account.to_string(), value.to_string());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<(), String> {
        self.0.lock().unwrap().remove(account);
        Ok(())
    }
}

/// 与某台节点配对的密钥在 Keychain 里的名字
pub fn node_psk_account(node_id: &str) -> String {
    format!("node_psk:{node_id}")
}
