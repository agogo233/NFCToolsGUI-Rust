//! 数据模型层：hex 编解码、dump 扇区结构、密钥表、工具输出解析。
//!
//! 全部为纯逻辑，零 GUI / 零 IO 依赖（`keys::save_keys` 是唯一例外，签名里显式收 `&Paths`）。
//!
//! 真相源是原版 `command.js` 的各 `processHandler` 与 `dumpEditor.js`；偏离登记见 docs/02 §6，
//! fixture 的构造方式与不可取证项见 docs/04 §2。

pub mod dump;
pub mod dumpcheck;
pub mod hex;
pub mod keys;
pub mod parse;
