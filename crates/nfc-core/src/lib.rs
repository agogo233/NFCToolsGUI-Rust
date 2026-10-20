//! nfc-core：NFCToolsGUI 纯逻辑核心，零 GUI 依赖。
//!
//! 模块随 S1-S7 逐步填充：
//! - S1 state / command / paths / i18n / theme
//! - S2 runner（ToolRunner trait）
//! - S3 serial（设备枚举与连接）
//! - S4 model（hex / dump / keys / parse）
//! - S5 tasks（detect / mfdetect）、data（registry）、dialogs
//! - S6+ tasks（hard_nested / dict_test / card_write）、selftest
//! - S10 dumpfiles（dump 目录的文件操作）、model::dump 的比较器 diff
//! - S11 engine（Action → ToolRunner 的编排层）

pub mod command;
pub mod data;
pub mod dialogs;
pub mod dumpfiles;
pub mod engine;
pub mod i18n;
pub mod model;
pub mod paths;
pub mod runner;
pub mod serial;
pub mod state;
pub mod tasks;
pub mod theme;
