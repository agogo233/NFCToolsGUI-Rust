//! i18n：中文文案表，编译期内嵌。
//!
//! 决策「仅中文」：不引入 `en.json` 与 `sys-locale`，无语言切换 UI。
//! 未知 key 原样返回 key 本身，对标原版缺失 key 的字面渲染行为。

use std::collections::HashMap;
use std::sync::OnceLock;

const ZH_CN: &str = include_str!("../../../assets/locales/zh-CN.json");

fn table() -> &'static HashMap<String, String> {
    static TABLE: OnceLock<HashMap<String, String>> = OnceLock::new();
    TABLE.get_or_init(|| serde_json::from_str(ZH_CN).unwrap_or_default())
}

/// 取 key 对应文案；未知 key 返回 key 本身。
pub fn t(key: &str) -> String {
    table().get(key).cloned().unwrap_or_else(|| key.to_owned())
}

/// 全部 key（排序后返回，便于稳定对比）。
pub fn keys() -> Vec<String> {
    let mut v: Vec<String> = table().keys().cloned().collect();
    v.sort();
    v
}

/// key 总数。
pub fn len() -> usize {
    table().len()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 不断言绝对数量——那个数字在 Rust 测试与 verify_parity.py 里各写了一份，
    /// 已经漂移过一次（凭空的 `html_license:` 被当成「原版拼写错误」保留）。
    /// 改为断言**构成**：本地补的 14 个必须都在，且不重名。
    #[test]
    fn zh_cn_has_the_local_extras_and_no_dupes() {
        const LOCAL: [&str; 14] = [
            "html_toggle_dark_mode",
            "html_toggle_light_mode",
            "html_follow_system_mode",
            "log_msg_hard_nested_no_progress",
            "log_msg_hard_nested_state_lost",
            "log_msg_nonces_empty_not_saved",
            "html_framework_bin_missing",
            "html_framework_bin_missing_short",
            // S15 UI 体验优化新增（原版均无对应 key）
            "html_switch_theme",
            "html_guidance_connect_first",
            "html_refresh_device_list",
            "html_event_key",
            "html_event_success",
            "html_event_error",
        ];
        let t = table();
        for k in LOCAL {
            assert!(t.contains_key(k), "本地补的 key 缺失: {k}");
        }
        // JSON 对象不会重名，但拼错的 key（如 `html_license:`）会静默多出来。
        // 这类「以为在补东西、其实建了个新 key」的错只有比对原版才发现得了，
        // 故这里钉住：不得出现以冒号结尾的 key。
        assert!(
            !t.keys().any(|k| k.ends_with(':')),
            "key 名不得以冒号结尾（原版没有这种拼法）: {:?}",
            t.keys().filter(|k| k.ends_with(':')).collect::<Vec<_>>()
        );
    }

    #[test]
    fn misspelled_keys_preserved_verbatim() {
        // 原版拼写错误 key 必须原样保留，不可「修正」
        for k in [
            "lod_msg_start_auto_hard_nested",
            "lod_msg_start_hard_nested",
            "log_msh_save_failed",
        ] {
            assert!(table().contains_key(k), "缺失原版拼写错误 key: {k}");
        }
        // 正确拼写版本不应同时存在
        assert!(!table().contains_key("log_msg_start_auto_hard_nested"));
        assert!(!table().contains_key("log_msg_start_hard_nested"));
        assert!(!table().contains_key("log_msg_save_failed"));
    }

    #[test]
    fn unknown_key_falls_back_to_key() {
        assert_eq!(t("definitely_not_a_key"), "definitely_not_a_key");
        assert_eq!(t(""), "");
    }

    #[test]
    fn known_key_resolves_to_chinese() {
        assert_eq!(t("indicator_connecting_device"), "正在连接设备");
        assert_eq!(t("dialog_msg_not_connected_device"), "请先连接设备");
        assert_eq!(t("dialog_msg_running_task"), "有任务进行中，不可执行");
        assert_eq!(t("app_name"), "NFCToolsGUI");
    }

    #[test]
    fn all_values_non_empty() {
        for (k, v) in table() {
            assert!(!v.trim().is_empty(), "空值 key: {k}",);
        }
    }

    #[test]
    fn keys_are_sorted_and_unique() {
        let ks = keys();
        assert_eq!(ks.len(), len());
        assert!(ks.windows(2).all(|w| w[0] < w[1]), "keys() 未排序");
        assert_eq!(
            ks.iter().collect::<std::collections::BTreeSet<_>>().len(),
            ks.len(),
            "存在重复 key"
        );
    }

    #[test]
    fn known_keys_resolve_to_different_text() {
        // 已知 key 必须解析成文案，不会原样返回
        for key in ["app_name", "indicator_free", "indicator_error"] {
            assert_ne!(t(key), key, "已知 key 应被解析: {key}");
        }
    }
}
