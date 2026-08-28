use std::collections::HashMap;
use tokio::sync::Mutex;

/// 按环境缓存的 JDK 布局（字段对齐 provision::package::ProvisionResult）
#[derive(Clone, Debug, PartialEq)]
pub struct JdkLayout {
    pub tool_home: String,
    pub bins: HashMap<String, String>,
}

/// 进程内缓存：env_id → JdkLayout。ensure_tool 成功时写入；
/// 执行遇 exit 127 / "No such file or directory" 时清除并引导重新 ensure_tool。
/// 不持久化——Friday 重启后为空，ensure_tool 幂等恢复。
#[derive(Default)]
pub struct JdkCache {
    layouts: Mutex<HashMap<String, JdkLayout>>,
}

impl JdkCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn set(&self, env_id: &str, layout: JdkLayout) {
        self.layouts.lock().await.insert(env_id.to_string(), layout);
    }

    pub async fn get(&self, env_id: &str) -> Option<JdkLayout> {
        self.layouts.lock().await.get(env_id).cloned()
    }

    pub async fn clear(&self, env_id: &str) {
        self.layouts.lock().await.remove(env_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> JdkLayout {
        let mut bins = HashMap::new();
        bins.insert("jcmd".to_string(), "/tmp/friday-tools/jdk-21.0.11/bin/jcmd".to_string());
        bins.insert("jstat".to_string(), "/tmp/friday-tools/jdk-21.0.11/bin/jstat".to_string());
        JdkLayout { tool_home: "/tmp/friday-tools/jdk-21.0.11".to_string(), bins }
    }

    #[tokio::test]
    async fn test_set_get_roundtrip() {
        let cache = JdkCache::new();
        cache.set("env-1", layout()).await;
        let got = cache.get("env-1").await.unwrap();
        assert_eq!(got, layout());
        assert_eq!(
            got.bins.get("jcmd").unwrap(),
            "/tmp/friday-tools/jdk-21.0.11/bin/jcmd"
        );
    }

    #[tokio::test]
    async fn test_get_missing_returns_none() {
        let cache = JdkCache::new();
        assert!(cache.get("nope").await.is_none());
    }

    #[tokio::test]
    async fn test_clear_removes_entry() {
        let cache = JdkCache::new();
        cache.set("env-1", layout()).await;
        cache.clear("env-1").await;
        assert!(cache.get("env-1").await.is_none());
    }

    #[tokio::test]
    async fn test_clear_missing_is_noop() {
        let cache = JdkCache::new();
        cache.clear("nope").await; // must not panic
    }

    #[tokio::test]
    async fn test_set_overwrites_previous_entry() {
        let cache = JdkCache::new();
        cache.set("env-1", layout()).await;
        let mut newer = layout();
        newer.tool_home = "/tmp/friday-tools/jdk-17.0.9".to_string();
        newer.bins.insert(
            "jcmd".to_string(),
            "/tmp/friday-tools/jdk-17.0.9/bin/jcmd".to_string(),
        );
        cache.set("env-1", newer.clone()).await;
        assert_eq!(cache.get("env-1").await.unwrap(), newer);
    }
}
