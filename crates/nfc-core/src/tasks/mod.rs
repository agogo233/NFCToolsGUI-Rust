//! 读卡、写卡与破解类任务实现。分层：
//! - [`detect`]：先 `nfc-mfdetect` 探测、再执行主命令的两段式编排
//! - [`mfoc`]：字典攻击 ETA 估算与 dump 落盘
//! - [`card_write`]：写卡 / 格式化 / 写 UFUID / 锁 UFUID 的参数与确认
//! - [`hard_nested`]：HardNested 破解状态机与 autoRun 递归
//! - [`dict_test`]：字典测试的参数、字典选择与命中落盘
//! - [`phw`]：手机/手环操作区——块号校验、改块与写 UID 位参数

pub mod card_write;
pub mod detect;
pub mod dict_test;
pub mod hard_nested;
pub mod mfoc;
pub mod phw;
