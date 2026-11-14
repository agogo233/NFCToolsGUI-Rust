//! 界面改版（docs/06）：三栏工作台的各栏实现。
//!
//! - [`ops_panel`]：左栏操作按钮组（读卡 / 破解 / 写卡 / 更多）
//! - [`workbench`]：中栏当前卡片档案 + 卡片扇区编辑器
//! - [`rail`]：右栏设备 / 当前任务 / 事件 / 控制台

pub mod ops_panel;
pub mod rail;
pub mod workbench;
