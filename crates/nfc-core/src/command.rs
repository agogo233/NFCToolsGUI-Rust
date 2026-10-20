//! 34 个 action 注册表与前置门禁（30 对齐原版 command.js + 4 个 S14 手机/手环增量）。
//!
//! action 名称与顺序严格对标原版 `src/command.js` 的 `actions` map。
//! 门禁对标 `execUtils.js::exec()` 的设备门禁与 `isRunningTask` 单任务限制。

use crate::state::AppState;
use crate::state::DeviceState;

/// 34 个 action 名称清单：前 30 顺序同原版 `command.js` actions map，
/// 末 4 项（phw-*）为 S14 手机/手环增量，原版 Electron 没有、仅 Tauri 端口实现。
pub const ALL_ACTIONS: &[&str] = &[
    "open-settings-window",
    "scan-usb-devices",
    "conn-usb-devices",
    "read-IC",
    "write-IC",
    "format-card",
    "input-keys-read-IC",
    "done-input-keys-read-IC",
    "detect-card-type",
    "write-ufuid-uid",
    "lock-ufuid",
    "hard-nested",
    "hard-nested-config-done",
    "run-hard-nested",
    "open-dict-file",
    "dict-test",
    "dict-test-config-done",
    "open-history-keys",
    "open-dump-editor",
    "dump-editor-choose-file",
    "dump-editor-save",
    "open-dump-comparator",
    "dump-comparator-choose-file",
    "open-dump-folder",
    "dump-history",
    "delete-dump",
    "rename-dump-file",
    "cancel-task",
    "save-log",
    "open-about",
    "open-phw-window",
    "phw-read-uid",
    "phw-write-uid",
    "phw-write-data",
];

/// 单个 action 的门禁声明。
pub struct ActionSpec {
    pub name: &'static str,
    /// 需设备已连接，否则 `NotConnected`。
    /// 例外：`conn-usb-devices` 内部运行 `nfc-list`，未连接时也要能点。
    pub connected: bool,
    /// 需无任务运行，否则 `Busy`。
    pub idle: bool,
    /// 连接过程中禁用（仅 `conn-usb-devices`）。
    pub not_while_connecting: bool,
}

/// 全量 action 注册表。
pub const ACTION_SPECS: &[ActionSpec] = &[
    ActionSpec {
        name: "open-settings-window",
        connected: false,
        idle: false,
        not_while_connecting: false,
    },
    ActionSpec {
        name: "scan-usb-devices",
        connected: false,
        idle: false,
        not_while_connecting: false,
    },
    ActionSpec {
        name: "conn-usb-devices",
        connected: false,
        idle: false,
        not_while_connecting: true,
    },
    ActionSpec {
        name: "read-IC",
        connected: true,
        idle: true,
        not_while_connecting: false,
    },
    ActionSpec {
        name: "write-IC",
        connected: true,
        idle: true,
        not_while_connecting: false,
    },
    ActionSpec {
        name: "format-card",
        connected: true,
        idle: true,
        not_while_connecting: false,
    },
    ActionSpec {
        name: "input-keys-read-IC",
        connected: false,
        idle: true,
        not_while_connecting: false,
    },
    ActionSpec {
        name: "done-input-keys-read-IC",
        connected: true,
        idle: true,
        not_while_connecting: false,
    },
    ActionSpec {
        name: "detect-card-type",
        connected: true,
        idle: true,
        not_while_connecting: false,
    },
    ActionSpec {
        name: "write-ufuid-uid",
        connected: true,
        idle: true,
        not_while_connecting: false,
    },
    ActionSpec {
        name: "lock-ufuid",
        connected: true,
        idle: true,
        not_while_connecting: false,
    },
    ActionSpec {
        name: "hard-nested",
        connected: false,
        idle: true,
        not_while_connecting: false,
    },
    ActionSpec {
        name: "hard-nested-config-done",
        connected: true,
        idle: true,
        not_while_connecting: false,
    },
    ActionSpec {
        name: "run-hard-nested",
        connected: true,
        idle: true,
        not_while_connecting: false,
    },
    ActionSpec {
        name: "open-dict-file",
        connected: false,
        idle: false,
        not_while_connecting: false,
    },
    ActionSpec {
        name: "dict-test",
        connected: false,
        idle: true,
        not_while_connecting: false,
    },
    ActionSpec {
        name: "dict-test-config-done",
        connected: true,
        idle: true,
        not_while_connecting: false,
    },
    ActionSpec {
        name: "open-history-keys",
        connected: false,
        idle: false,
        not_while_connecting: false,
    },
    ActionSpec {
        name: "open-dump-editor",
        connected: false,
        idle: false,
        not_while_connecting: false,
    },
    ActionSpec {
        name: "dump-editor-choose-file",
        connected: false,
        idle: false,
        not_while_connecting: false,
    },
    ActionSpec {
        name: "dump-editor-save",
        connected: false,
        idle: false,
        not_while_connecting: false,
    },
    ActionSpec {
        name: "open-dump-comparator",
        connected: false,
        idle: false,
        not_while_connecting: false,
    },
    ActionSpec {
        name: "dump-comparator-choose-file",
        connected: false,
        idle: false,
        not_while_connecting: false,
    },
    ActionSpec {
        name: "open-dump-folder",
        connected: false,
        idle: false,
        not_while_connecting: false,
    },
    ActionSpec {
        name: "dump-history",
        connected: false,
        idle: false,
        not_while_connecting: false,
    },
    ActionSpec {
        name: "delete-dump",
        connected: false,
        idle: false,
        not_while_connecting: false,
    },
    ActionSpec {
        name: "rename-dump-file",
        connected: false,
        idle: false,
        not_while_connecting: false,
    },
    ActionSpec {
        name: "cancel-task",
        connected: false,
        idle: false,
        not_while_connecting: false,
    },
    ActionSpec {
        name: "save-log",
        connected: false,
        idle: false,
        not_while_connecting: false,
    },
    ActionSpec {
        name: "open-about",
        connected: false,
        idle: false,
        not_while_connecting: false,
    },
    // S14 手机/手环增量：open-phw-window 纯 UI 开关，三个 phw-* 都要设备连接且空闲。
    ActionSpec {
        name: "open-phw-window",
        connected: false,
        idle: false,
        not_while_connecting: false,
    },
    ActionSpec {
        name: "phw-read-uid",
        connected: true,
        idle: true,
        not_while_connecting: false,
    },
    ActionSpec {
        name: "phw-write-uid",
        connected: true,
        idle: true,
        not_while_connecting: false,
    },
    ActionSpec {
        name: "phw-write-data",
        connected: true,
        idle: true,
        not_while_connecting: false,
    },
];

/// 按名称查注册表项。
pub fn spec(name: &str) -> Option<&'static ActionSpec> {
    ACTION_SPECS.iter().find(|s| s.name == name)
}

/// 前置门禁失败原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateError {
    /// 未连接设备
    NotConnected,
    /// 已有任务运行
    Busy,
    /// 连接进行中
    Connecting,
}

impl GateError {
    /// 对应 i18n key，均已存在于 zh-CN.json。
    pub fn i18n_key(self) -> &'static str {
        match self {
            GateError::NotConnected => "dialog_msg_not_connected_device",
            GateError::Busy => "dialog_msg_running_task",
            GateError::Connecting => "indicator_connecting_device",
        }
    }
}

/// 前置门禁检查。对标原版 `exec()` 的未连接拦截与 `isRunningTask` 单任务限制。
pub fn gate(spec: &ActionSpec, state: &AppState) -> Result<(), GateError> {
    if spec.not_while_connecting && state.device_state == DeviceState::Connecting {
        return Err(GateError::Connecting);
    }
    if spec.connected && !state.is_device_connected {
        return Err(GateError::NotConnected);
    }
    if spec.idle && state.is_running_task {
        return Err(GateError::Busy);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n;

    #[test]
    fn all_actions_is_exactly_34() {
        assert_eq!(ALL_ACTIONS.len(), 34);
    }

    #[test]
    fn all_action_names_are_unique() {
        let mut v: Vec<&str> = ALL_ACTIONS.to_vec();
        v.sort_unstable();
        v.dedup();
        assert_eq!(v.len(), ALL_ACTIONS.len(), "ALL_ACTIONS 存在重复 action 名");
    }

    #[test]
    fn registry_covers_every_action() {
        assert_eq!(ACTION_SPECS.len(), ALL_ACTIONS.len());
        for name in ALL_ACTIONS {
            assert!(spec(name).is_some(), "注册表缺少 action: {name}");
        }
    }

    #[test]
    fn spec_names_match_all_actions_in_order() {
        assert_eq!(
            ACTION_SPECS.iter().map(|s| s.name).collect::<Vec<_>>(),
            ALL_ACTIONS,
            "注册表顺序应与 ALL_ACTIONS 一致（顺序即原版 actions map 顺序）"
        );
    }

    #[test]
    fn spec_lookup_rejects_unknown() {
        assert!(spec("no-such-action").is_none());
        assert!(spec("").is_none());
    }

    #[test]
    fn connected_actions_match_spec_doc() {
        // docs/01 §3 标注「连接」前置的 10 个 action
        let want: &[&str] = &[
            "read-IC",
            "write-IC",
            "format-card",
            "done-input-keys-read-IC",
            "detect-card-type",
            "write-ufuid-uid",
            "lock-ufuid",
            "hard-nested-config-done",
            "run-hard-nested",
            "dict-test-config-done",
            "phw-read-uid",
            "phw-write-uid",
            "phw-write-data",
        ];
        let got: Vec<&str> = ACTION_SPECS
            .iter()
            .filter(|s| s.connected)
            .map(|s| s.name)
            .collect();
        assert_eq!(got, want, "连接门禁集合与 docs/01 §3 不符");
    }

    #[test]
    fn idle_actions_match_spec_doc() {
        // docs/01 §3「前置」列标为 idle 的全部 action，按注册表顺序
        let want: &[&str] = &[
            "read-IC",
            "write-IC",
            "format-card",
            "input-keys-read-IC",
            "done-input-keys-read-IC",
            "detect-card-type",
            "write-ufuid-uid",
            "lock-ufuid",
            "hard-nested",
            "hard-nested-config-done",
            "run-hard-nested",
            "dict-test",
            "dict-test-config-done",
            "phw-read-uid",
            "phw-write-uid",
            "phw-write-data",
        ];
        let got: Vec<&str> = ACTION_SPECS
            .iter()
            .filter(|s| s.idle)
            .map(|s| s.name)
            .collect();
        assert_eq!(got, want, "空闲门禁集合与 docs/01 §3 不符");
    }

    #[test]
    fn only_connect_action_is_gated_while_connecting() {
        let gated: Vec<&str> = ACTION_SPECS
            .iter()
            .filter(|s| s.not_while_connecting)
            .map(|s| s.name)
            .collect();
        assert_eq!(gated, vec!["conn-usb-devices"]);
    }

    #[test]
    fn gate_matrix() {
        let s_read = spec("read-IC").unwrap();
        let s_conn = spec("conn-usb-devices").unwrap();

        // 未连接 → NotConnected
        let idle = AppState::new();
        assert_eq!(gate(s_read, &idle), Err(GateError::NotConnected));

        // 已连接且空闲 → 通过
        let mut ok = AppState::new();
        ok.is_device_connected = true;
        assert_eq!(gate(s_read, &ok), Ok(()));

        // 已连接但运行中 → Busy
        ok.is_running_task = true;
        assert_eq!(gate(s_read, &ok), Err(GateError::Busy));

        // 连接中 → conn-usb-devices 被拒
        ok.is_running_task = false;
        ok.device_state = DeviceState::Connecting;
        assert_eq!(gate(s_conn, &ok), Err(GateError::Connecting));

        // 连接结束 → conn-usb-devices 放行
        ok.device_state = DeviceState::None;
        assert_eq!(gate(s_conn, &ok), Ok(()));
    }

    #[test]
    fn always_available_actions_ignore_all_gates() {
        let mut s = AppState::new();
        s.is_running_task = true;
        s.device_state = DeviceState::Connecting;

        for name in [
            "open-settings-window",
            "scan-usb-devices",
            "cancel-task",
            "save-log",
            "open-about",
            "open-dict-file",
            "delete-dump",
            "dump-history",
        ] {
            assert_eq!(
                gate(spec(name).unwrap(), &s),
                Ok(()),
                "{name} 不受任何门禁限制"
            );
        }
    }

    #[test]
    fn idle_only_actions_pass_when_connected_is_false() {
        // input-keys-read-IC 仅需空闲，未连接也可打开面板
        let s = AppState::new();
        assert_eq!(gate(spec("input-keys-read-IC").unwrap(), &s), Ok(()));
        assert_eq!(gate(spec("hard-nested").unwrap(), &s), Ok(()));
        assert_eq!(gate(spec("dict-test").unwrap(), &s), Ok(()));
    }

    #[test]
    fn gate_error_i18n_keys_exist() {
        for e in [
            GateError::NotConnected,
            GateError::Busy,
            GateError::Connecting,
        ] {
            let key = e.i18n_key();
            assert_ne!(i18n::t(key), key, "门禁提示 key 不存在于 zh-CN.json: {key}");
        }
        assert_eq!(i18n::t(GateError::NotConnected.i18n_key()), "请先连接设备");
        assert_eq!(
            i18n::t(GateError::Busy.i18n_key()),
            "有任务进行中，不可执行"
        );
        assert_eq!(i18n::t(GateError::Connecting.i18n_key()), "正在连接设备");
    }

    #[test]
    fn total_gated_actions() {
        let gated = ACTION_SPECS
            .iter()
            .filter(|s| s.connected || s.idle || s.not_while_connecting)
            .count();
        assert_eq!(gated, 17, "13 连接 + 16 空闲 + 1 连接中，去重后应为 17");
    }
}
