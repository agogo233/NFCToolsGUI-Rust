//! 路径表：数据目录、临时文件、默认文件名、时间戳。
//!
//! 对标原版 `src/command.js` 的路径常量与 `getTimeList()`。
//! 全部为纯函数，可单测；不依赖 GUI 与真实文件系统。

use std::path::{Path, PathBuf};

/// 应用名，同时作为数据目录名。
pub const APP_NAME: &str = "NFCToolsGUI";

/// 串口波特率，原版恒为 115200。
pub const DEVICE_SPEED: u32 = 115_200;

/// 数据目录与派生路径。`base` 可注入，便于单测指向临时目录。
#[derive(Clone, Debug)]
pub struct Paths {
    base: PathBuf,
}

impl Paths {
    pub fn new(base: impl Into<PathBuf>) -> Self {
        Self { base: base.into() }
    }

    /// 运行期数据目录：`BaseDirs::data_dir()/NFCToolsGUI`（Windows 为 `%APPDATA%`）。
    pub fn from_app_data() -> Self {
        let base = directories::BaseDirs::new()
            .map(|b| b.data_dir().join(APP_NAME))
            .unwrap_or_else(|| std::env::temp_dir().join(APP_NAME));
        Self::new(base)
    }

    pub fn base(&self) -> &Path {
        &self.base
    }

    /// 已知密钥表，每行一个 12 位 hex。
    pub fn keys_file(&self) -> PathBuf {
        self.base.join("keys.txt")
    }

    /// 读卡临时 dump，任务结束后删除。
    pub fn temp_mfd(&self) -> PathBuf {
        self.base.join("temp.mfd")
    }

    pub fn dumpfiles_dir(&self) -> PathBuf {
        self.base.join("dumpfiles")
    }

    /// hard-nested 采集输出。
    pub fn nonces_file(&self) -> PathBuf {
        self.base.join("nonces.bin")
    }

    pub fn nfc_config(&self) -> PathBuf {
        self.base.join("libnfc.conf")
    }

    pub fn registry_file(&self) -> PathBuf {
        self.base.join("registry.json")
    }

    /// `libnfc.conf` 内容；`device` 为串口路径，`speed` 恒 115200。
    pub fn libnfc_conf(device: &str, speed: u32) -> String {
        format!("device.name = \"NFC_Device\"\ndevice.connstring = \"pn532_uart:{device}:{speed}\"")
    }

    /// dump 默认文件名：`{uid}_{timestamp}.mfd`；uid 为空时退化为 `card`。
    pub fn dump_file_name(uid: &str, stamp: &str) -> String {
        let uid = uid.trim();
        let uid = if uid.is_empty() { "card" } else { uid };
        format!("{uid}_{stamp}.mfd")
    }

    /// 日志默认文件名：`NFCTools_log_{timestamp}.txt`。
    pub fn log_file_name(stamp: &str) -> String {
        format!("NFCTools_log_{stamp}.txt")
    }
}

/// `dict.dic` 默认字典：打包时取 exe 同目录，开发时取仓库根。
pub fn dict_file() -> PathBuf {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(PathBuf::from));
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(PathBuf::from)
        .unwrap_or_default();
    dict_file_from(exe_dir.as_deref(), &repo_root)
}

/// 纯函数版：`exe_dir` 为 exe 所在目录（打包形态），`repo_root` 为仓库根（开发形态）。
pub fn dict_file_from(exe_dir: Option<&Path>, repo_root: &Path) -> PathBuf {
    if let Some(dir) = exe_dir {
        let packed = dir.join("dict.dic");
        // 打包形态：dict.dic 与 exe 同级；或 framework 目录存在（用户尚未补齐 bin/）。
        if packed.exists() || dir.join("framework").exists() {
            return packed;
        }
    }
    let repo = repo_root.join("dict.dic");
    if repo.exists() {
        return repo;
    }
    // `CARGO_MANIFEST_DIR` 是**编译机**的绝对路径，发布版里它指向用户机上
    // 不存在的目录。与其把「缺字典」指向一个编造的编译机路径，不如指回
    // exe 旁的预期位置，让读字典时的报错出现在正确的地点。
    exe_dir.map(|d| d.join("dict.dic")).unwrap_or(repo)
}

/// 当前时间戳 `yyyy_MM_dd_HH_mm_ss`。
///
/// 对标 `command.js::getTimeList()`。原版取**本地时区**（`getHours()`），
/// 而 Rust 标准库无时区支持，故此处固定用 UTC——与本地时区差 8 小时
/// （东八区）。**已披露于 README §已知限制**；完全对齐需引入时区库，
/// 本版刻意不做（影响仅文件名/日志名，无正确性问题）。
pub fn timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    format_timestamp(secs)
}

/// 纯函数：unix 秒 → `yyyy_MM_dd_HH_mm_ss`。
pub fn format_timestamp(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(86_400);
    let rem = unix_secs.rem_euclid(86_400);
    let hh = rem / 3_600;
    let mm = (rem % 3_600) / 60;
    let ss = rem % 60;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}_{month:02}_{day:02}_{hh:02}_{mm:02}_{ss:02}")
}

/// Unix 纪元天数 → 公历日期。
///
/// 实现自 Howard Hinnant 的 proleptic Gregorian 转换。
/// 注意两处易错点：`1/2` 月需回加一年；`d` 的常数为 `+2` 而非 `-455`。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    if month <= 2 {
        year += 1;
    }
    (year, month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_timestamp_epoch() {
        assert_eq!(format_timestamp(0), "1970_01_01_00_00_00");
    }

    #[test]
    fn format_timestamp_known_values() {
        assert_eq!(format_timestamp(60), "1970_01_01_00_01_00");
        assert_eq!(format_timestamp(86_399), "1970_01_01_23_59_59");
        assert_eq!(format_timestamp(86_400), "1970_01_02_00_00_00");
        assert_eq!(format_timestamp(31_536_000), "1971_01_01_00_00_00");
        // 闰年边界：2024 有 2/29
        assert_eq!(format_timestamp(1_704_067_200), "2024_01_01_00_00_00");
        assert_eq!(format_timestamp(1_706_745_600), "2024_02_01_00_00_00");
        assert_eq!(format_timestamp(1_709_078_400), "2024_02_28_00_00_00");
        assert_eq!(format_timestamp(1_709_164_800), "2024_02_29_00_00_00");
        assert_eq!(format_timestamp(1_709_251_200), "2024_03_01_00_00_00");
        // 100 年不闰：1900 无 2/29
        assert_eq!(format_timestamp(-2_203_977_600), "1900_02_28_00_00_00");
        assert_eq!(format_timestamp(-2_203_891_200), "1900_03_01_00_00_00");
        // 400 年闰：2000 有 2/29
        assert_eq!(format_timestamp(951_782_400), "2000_02_29_00_00_00");
    }

    #[test]
    fn format_timestamp_negative() {
        assert_eq!(format_timestamp(-1), "1969_12_31_23_59_59");
        assert_eq!(format_timestamp(-86_400), "1969_12_31_00_00_00");
    }

    #[test]
    fn timestamp_shape_is_19_chars() {
        let t = timestamp();
        assert_eq!(t.len(), 19, "实际: {t}");
        let b = t.as_bytes();
        for i in [4, 7, 10, 13, 16] {
            assert_eq!(b[i], b'_', "位置 {i} 应为下划线，实际: {t}");
        }
    }

    #[test]
    fn civil_from_days_known_dates() {
        // 表驱动：覆盖纪元首日、年份回加（1/2 月）、4 年/400 年闰周期、负值
        let table = [
            (0i64, (1970, 1, 1u32)),
            (1i64, (1970, 1, 2)),
            (3i64, (1970, 1, 4)),
            (365i64, (1971, 1, 1)),
            (366i64, (1971, 1, 2)),
            (1000i64, (1972, 9, 27)),
            (1461i64, (1974, 1, 1)),
            (36_524i64, (2069, 12, 31)),
            (36_525i64, (2070, 1, 1)),
            (146_096i64, (2369, 12, 31)),
            (146_097i64, (2370, 1, 1)),
            (-1i64, (1969, 12, 31)),
            (-2i64, (1969, 12, 30)),
            (-100_000i64, (1696, 3, 17)),
        ];
        for (days, want) in table {
            assert_eq!(civil_from_days(days), want, "days={days}");
        }
    }

    #[test]
    fn paths_layout() {
        let p = Paths::new("/data/NFCToolsGUI");
        assert_eq!(p.base(), Path::new("/data/NFCToolsGUI"));
        assert_eq!(p.keys_file(), PathBuf::from("/data/NFCToolsGUI/keys.txt"));
        assert_eq!(p.temp_mfd().file_name().unwrap(), "temp.mfd");
        assert_eq!(p.dumpfiles_dir().file_name().unwrap(), "dumpfiles");
        assert_eq!(p.nonces_file().file_name().unwrap(), "nonces.bin");
        assert_eq!(p.nfc_config().file_name().unwrap(), "libnfc.conf");
        assert_eq!(p.registry_file().file_name().unwrap(), "registry.json");
    }

    #[test]
    fn libnfc_conf_content() {
        assert_eq!(
            Paths::libnfc_conf("/dev/ttyACM0", DEVICE_SPEED),
            "device.name = \"NFC_Device\"\ndevice.connstring = \"pn532_uart:/dev/ttyACM0:115200\""
        );
    }

    #[test]
    fn dump_name_falls_back_to_card() {
        assert_eq!(
            Paths::dump_file_name("", "2024_01_01_00_00_00"),
            "card_2024_01_01_00_00_00.mfd"
        );
        assert_eq!(Paths::dump_file_name("   ", "x"), "card_x.mfd");
        assert_eq!(Paths::dump_file_name("  AABBCCDD  ", "x"), "AABBCCDD_x.mfd");
    }

    #[test]
    fn log_name() {
        assert_eq!(
            Paths::log_file_name("2024_01_01_00_00_00"),
            "NFCTools_log_2024_01_01_00_00_00.txt"
        );
    }

    #[test]
    fn from_app_data_uses_app_data_dir() {
        let p = Paths::from_app_data();
        assert!(
            p.base().ends_with(APP_NAME),
            "base 应终止于 {APP_NAME}，实际: {:?}",
            p.base()
        );
    }

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .map(PathBuf::from)
            .unwrap()
    }

    #[test]
    fn dict_file_from_falls_back_to_repo_root() {
        let root = repo_root();
        let p = dict_file_from(None, &root);
        assert!(p.ends_with("dict.dic"), "实际: {p:?}");
        assert!(p.starts_with(&root), "实际: {p:?}");
        // 仓库根确实带有一份字典
        assert!(p.is_file(), "仓库根缺 dict.dic: {p:?}");
    }

    #[test]
    fn dict_file_from_prefers_exe_dir_when_packaged() {
        let tmp = std::env::temp_dir().join("nfc-core-dict-test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("framework/bin")).unwrap();

        let p = dict_file_from(Some(&tmp), Path::new("/nonexistent"));
        assert!(p.starts_with(&tmp), "打包态应取 exe 目录，实际: {p:?}");
        assert!(p.ends_with("dict.dic"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn dict_file_wrapper_is_usable() {
        // 仅验证当前运行形态下不 panic、路径以 dict.dic 结尾
        let p = dict_file();
        assert!(p.ends_with("dict.dic"), "实际: {p:?}");
    }
}
