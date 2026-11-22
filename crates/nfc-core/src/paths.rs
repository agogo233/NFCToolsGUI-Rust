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
            .unwrap_or_else(Self::fallback_base);
        Self::new(base)
    }

    /// `BaseDirs` 拿不到（HOME 未设等）时的兜底目录。
    ///
    /// 不能直接用共享的 `temp/NFCToolsGUI`——那是**可预测的共享路径**，同机
    /// 其他用户可预建目录窥探/顶替。这里拼上当前用户名（取不到再用进程号）
    /// 做隔离，并告警提示环境异常。
    fn fallback_base() -> PathBuf {
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .ok();
        let name = fallback_dir_suffix(user.as_deref(), std::process::id());
        eprintln!("warning: home directory unavailable, data dir falls back to temp");
        std::env::temp_dir().join(format!("{APP_NAME}-{name}"))
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

    /// `libnfc.conf` 内容；`device` 为串口路径，`speed` 由调用方指定（S18 可调）。
    pub fn libnfc_conf(device: &str, speed: u32) -> String {
        format!("device.name = \"NFC_Device\"\ndevice.connstring = \"pn532_uart:{device}:{speed}\"")
    }

    /// CUID 空卡密钥文件（`cuid-write` 的第二段用，收尾时删除）。
    pub fn cuid_keyfile(&self) -> PathBuf {
        self.base.join("cuid.mfd")
    }

    /// dump 默认文件名：`{uid}_{timestamp}.mfd`；uid 为空时退化为 `card`。
    ///
    /// **`uid` 来自 `mfoc` 的 stdout**（`take_uid` 只去空白，不校验字符集），
    /// 而本函数的返回值会被 `dir.join()` 拼成落盘路径。任意读卡器都能打印
    /// 一行伪造的 `UID (NFCID1): ../../x`，故此处必须把「不是纯 hex 的 uid」
    /// 整体退化为 `card`——否则默认名可逃出 `dumpfiles/` 覆盖任意文件
    /// （纵深防御第二道；第一道是 `dumpfiles::is_plain_name`）。
    pub fn dump_file_name(uid: &str, stamp: &str) -> String {
        let uid = uid.trim();
        let uid = if uid.is_empty() || !uid.chars().all(|c| c.is_ascii_hexdigit()) {
            "card"
        } else {
            uid
        };
        format!("{uid}_{stamp}.mfd")
    }

    /// 编辑器「写入此转储」的副本名：`{原 stem}_edit_{timestamp}.mfd`。
    ///
    /// 写卡前先另存一份，原始解卡结果不被覆盖（见 app 侧
    /// `write_editor_to_card`）。扩展名恒为 `.mfd`：历史面板只列 `.mfd`
    /// （`dumpfiles::list`），换个扩展名副本就会在那儿失踪、删不掉。
    pub fn edited_copy_name(name: &str, stamp: &str) -> String {
        let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
        let stem = stem.trim();
        let stem = if stem.is_empty() { "dump" } else { stem };
        format!("{stem}_edit_{stamp}.mfd")
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

/// 把敏感产物（密钥表 / nonce / CUID 密钥文件）收紧为「仅本人可读」。
///
/// Unix 上置 `0o600`：`fs::write` 的默认 mode 是 `0666 & ~umask`（常见
/// umask 022 → 0644，同机其他用户可读），密码学产物不应如此。
/// **注意**：`rename` 会保留 mode，所以 keys.rs 的「写 tmp 再 rename」路径
/// 必须在 rename **之前**对 tmp 调用本函数。
///
/// Windows 上为空操作：无 umask 概念，标准库的 `set_permissions` 只映射
/// 只读位、无法表达 `rw-------`；而 `%APPDATA%` 默认 ACL 本就仅限当前用户
/// 与管理员，实际暴露面小于 Unix 多用户机器。该语义差异登记于 docs/02
/// §6.31；若将来要真正收紧需 NTFS ACL（外部 crate），不在本仓范围内。
#[cfg(unix)]
pub fn restrict_owner(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
pub fn restrict_owner(_path: &Path) {}

/// 把数据目录收紧为「仅本人可进」（`0700`）。
///
/// 目录里的文件名含卡片 UID 与时间戳，目录可列（默认 `0755`）等于向同机
/// 其他用户泄露「读过哪些卡、何时读的」。文件本体已由 [`restrict_owner`]
/// 收紧，这里补上目录这一层。
///
/// Windows 上为空操作，理由同 [`restrict_owner`]（§6.31）。
#[cfg(unix)]
pub fn restrict_owner_dir(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
pub fn restrict_owner_dir(_path: &Path) {}

/// 原子写「仅本人可读」的文件：先写同目录 `.tmp`，收紧后 `rename` 替换。
///
/// 与 `model/keys.rs` / `data.rs` 的「写 tmp → restrict → rename」三步同一
/// 策略，收拢成公共函数供 app 侧（编辑器写回等）复用——崩溃时不留半截
/// 文件。`.tmp` 与目标同目录，`rename` 必在同文件系统内；目标已存在时
/// `rename` 原子替换（Windows 侧 `fs::rename` 带 REPLACE_EXISTING 语义，
/// 与 keys/registry 既有用法一致）。
pub fn write_private_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = PathBuf::from(format!("{}.tmp", path.display()));
    std::fs::write(&tmp, bytes)?;
    restrict_owner(&tmp);
    if let Err(e) = std::fs::rename(&tmp, path) {
        // rename 失败（Windows 目标被占用、父目录被删等）不留 .tmp 垃圾——
        // 「另存为」到用户自选目录时它会成了可见的残留文件。
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// `fallback_base` 的目录名后缀：优先净化后的用户名，取不到用进程号。
///
/// 净化规则：只留 ASCII 字母/数字/下划线/连字符，截断到 32 字符——用户名
/// 可能含域分隔符 `\`、`@` 或空格，直接拼路径不安全。
fn fallback_dir_suffix(user: Option<&str>, pid: u32) -> String {
    let clean = user
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(32)
        .collect::<String>();
    if clean.is_empty() {
        format!("pid{pid}")
    } else {
        clean
    }
}

/// 当前时间戳 `yyyy_MM_dd_HH_mm_ss`（**本地时区**）。
///
/// 对标 `command.js::getTimeList()` 的本地时区语义。S18（F3）修正：
/// 此前用 UTC（§6.23 登记的「刻意不做」），东八区用户看到的文件名早 8 小时。
/// 现按本地时区偏移渲染，偏移来自 C 库（Windows `GetLocalTime` / POSIX
/// `localtime_r`），零新增依赖。
pub fn timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    format_timestamp(secs + local_offset_secs())
}

/// 事件条时间戳 `HH:MM:SS`（**本地时区**，对标原版 `index.js:118` 的
/// `getHours()`）。`app::event_stamp` 委托到此。
pub fn event_stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let sod = (secs + local_offset_secs()).rem_euclid(86_400);
    format!("{:02}:{:02}:{:02}", sod / 3600, (sod % 3600) / 60, sod % 60)
}

/// 本地时区相对 UTC 的偏移（秒），进程内只算一次。
///
/// 取「同一瞬间」的本地钟与 UTC 钟之差，归一化到 `(-86400, 86400]`。
/// 拿不到（C 库调用失败）时按 0 处理（退回 UTC，与旧行为一致）。
fn local_offset_secs() -> i64 {
    static OFFSET: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    *OFFSET.get_or_init(detect_local_offset_secs)
}

/// 平台的本地偏移探测。
#[cfg(windows)]
fn detect_local_offset_secs() -> i64 {
    #[repr(C)]
    struct SysTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        milliseconds: u32,
    }
    extern "system" {
        fn GetLocalTime(out: *mut SysTime) -> u32;
        fn GetSystemTime(out: *mut SysTime) -> u32;
    }
    let mut local = std::mem::MaybeUninit::uninit();
    let mut utc = std::mem::MaybeUninit::uninit();
    if unsafe { GetLocalTime(local.as_mut_ptr()) == 0 || GetSystemTime(utc.as_mut_ptr()) == 0 } {
        return 0;
    }
    let local = unsafe { local.assume_init() };
    let utc = unsafe { utc.assume_init() };
    // **必须用到日期**，不能只比「时分秒」。偏移跨过午夜时，钟点差会与
    // 真实偏移差整整一天且**无法区分**：UTC+14 在 UTC 12:00 时本地是次日
    // 02:00，钟点差 -36000，而 UTC-10 同为 -36000。
    //
    // 故与 POSIX 分支同法：先算「年内秒数」（含 day-of-year），相减后再归一。
    // Windows 的 `SYSTEMTIME` 给的是 year/month/day，这里就地折算成
    // 年积日，避免再引 `tm` 结构。
    let soy = |t: &SysTime| -> i64 {
        (days_from_civil(t.year as i64, t.month as i64, t.day as i64) * 86_400)
            + i64::from(t.hour) * 3600
            + i64::from(t.minute) * 60
            + i64::from(t.second)
    };
    normalize_offset(soy(&local) - soy(&utc))
}

/// 公历日期 → 自 1970-01-01 起的天数（Howard Hinnant 的 `days_from_civil`）。
///
/// 只用整数运算、无查表、正确处理闰年与世纪规则。仅被 Windows 时区探测使用。
#[cfg_attr(not(windows), allow(dead_code))]
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = (m + 9) % 12; // 3 月为 0
    let doy = (153 * mp + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// 平台的本地偏移探测（POSIX：C 库 `localtime_r` / `gmtime_r`）。
#[cfg(not(windows))]
fn detect_local_offset_secs() -> i64 {
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Tm {
        sec: i32,
        min: i32,
        hour: i32,
        mday: i32,
        mon: i32,
        year: i32,
        wday: i32,
        yday: i32,
        isdst: i32,
        // glibc 的 `struct tm` 在 9 个 int 之后还有 `tm_gmtoff`（long）与
        // `tm_zone`（char*）——`localtime_r` 会**完整**写入，缺了这两个
        // 字段会让 C 库越过本结构体的边界写坏栈内存（曾致 CI 随机 SIGSEGV），
        // 故必须逐一声明、尺寸与 glibc 对齐。
        gmtoff: i64,
        zone: *const u8,
    }
    extern "C" {
        fn localtime_r(timep: *const i64, result: *mut Tm) -> *mut Tm;
        fn gmtime_r(timep: *const i64, result: *mut Tm) -> *mut Tm;
    }
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut local = Tm {
        sec: 0,
        min: 0,
        hour: 0,
        mday: 0,
        mon: 0,
        year: 0,
        wday: 0,
        yday: 0,
        isdst: 0,
        gmtoff: 0,
        zone: std::ptr::null(),
    };
    let mut utc = local;
    unsafe {
        if localtime_r(&secs, &mut local).is_null() || gmtime_r(&secs, &mut utc).is_null() {
            return 0;
        }
    };
    // **不能只用 `yday` 相减**：该差值是「按当年天数计的秒数」，跨年时两侧
    // yday 相差近一整年（12/31 的 364 与 1/1 的 0），`normalize_offset` 按 ±1 天
    // 回绕无法还原，得到的是乱值（实测 UTC+14 在 12/31 23:00 UTC 时算成 -36000）。
    //
    // 优先采用 glibc/POSIX 的 `tm_gmtoff`：它直接给出该刻相对 UTC 的偏移，
    // 与日期无关，最可靠。musl 等不保证该字段（值任意），故只在其落在合法
    // 区间时才信；否则走下面的绝对天差算法（两者结论一致）。
    if (MIN_TZ_OFFSET..=MAX_TZ_OFFSET).contains(&local.gmtoff) {
        return local.gmtoff;
    }
    // 回落：用「绝对日期的天序号 + 当日时刻」相减。
    //
    // **不能用 `yday`**：它是「当年第几天」，跨年时 12/31 的 364 与 1/1 的 0
    // 相差近一整年，`normalize_offset` 按 ±1 天回绕还原不出来（UTC+14 会算成
    // -36000）。`days_from_civil` 给的是自 1970-01-01 起的连续天序号，
    // 跨年/跨闰日都自然正确，与 Windows 分支同一算法。
    let soy = |t: &Tm| -> i64 {
        days_from_civil(
            i64::from(t.year) + 1900,
            i64::from(t.mon) + 1,
            i64::from(t.mday),
        ) * 86_400
            + i64::from(t.hour) * 3600
            + i64::from(t.min) * 60
            + i64::from(t.sec)
    };
    normalize_offset(soy(&local) - soy(&utc))
}

/// 时区偏移上限：UTC+14（`Pacific/Kiritimati`）是现实中的最大值 50400 秒。
///
/// 区间必须覆盖 `[-12:00, +14:00]`。此前对称压缩到 `±12:00`（43200），
/// 把 UTC+12:45…UTC+14 的**真实**偏移误判成「跨了一整日」而减去 86400，
/// 得到 -36000 之类的负偏移——这些时区的文件名时间戳整整差一天
/// （实测 `TZ=Pacific/Kiritimati` 比系统 `date` 早一天）。
const MAX_TZ_OFFSET: i64 = 50_400;
/// 时区偏移下限：UTC-12 = -43200。
const MIN_TZ_OFFSET: i64 = -43_200;

/// 把「同刻本地/UTC 之差」归一化到 `(MIN_TZ_OFFSET, MAX_TZ_OFFSET]`
/// （跨年 / 跨午夜时差一整日）。
///
/// 不对称区间是**必须**的：合法偏移集是 `[-43200, 50400]`，
/// 用对称的 `±43200` 会把 UTC+12:45 以上的时区判成需要回绕。
fn normalize_offset(mut d: i64) -> i64 {
    while d <= MIN_TZ_OFFSET {
        d += 86_400;
    }
    while d > MAX_TZ_OFFSET {
        d -= 86_400;
    }
    d
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
    fn normalize_offset_wraps_across_day_boundary() {
        assert_eq!(normalize_offset(0), 0);
        assert_eq!(normalize_offset(8 * 3600), 8 * 3600, "东八区");
        assert_eq!(normalize_offset(-5 * 3600), -5 * 3600, "西五区");
        // 跨年瞬间差一整天：-82800（= 3600 - 86400）应归一回 3600
        assert_eq!(normalize_offset(3600 - 86_400), 3600);
        assert_eq!(normalize_offset(-3600 + 86_400), -3600);
    }

    /// 回归：合法时区上界 UTC+14（50400）此前被对称区间 `±43200` 误回绕成
    /// -36000，导致这些时区的文件名时间戳差一整天。
    #[test]
    fn normalize_offset_keeps_utc_plus_12_45_through_14() {
        assert_eq!(normalize_offset(50_400), 50_400, "UTC+14 应原样保留");
        assert_eq!(normalize_offset(45_900), 45_900, "UTC+12:45");
        assert_eq!(normalize_offset(49_500), 49_500, "UTC+13:45");
        assert_eq!(normalize_offset(-43_200), 43_200, "UTC-12 回绕为正 12:00");
        assert_eq!(normalize_offset(50_401), 50_401 - 86_400, "越过上界才回绕");
    }

    /// 回归：跨年瞬间 `yday` 相减会差近一整年（12/31 的 364 与 1/1 的 0），
    /// 按 ±1 天回绕**还原不出来**，UTC+14 会被算成 -36000（时间戳差一整天）。
    /// 正确值应由该刻的真实偏移给出（POSIX 走 `tm_gmtoff`，Windows 走
    /// `days_from_civil` 的绝对天差）。
    #[test]
    fn year_boundary_offset_is_not_corrupted_by_yday_wrap() {
        // 用绝对天差构造：12/31 23:00 UTC 对应的本地时刻是次年 1/1 13:00(+14)
        let utc_day = days_from_civil(2025, 12, 31);
        let loc_day = days_from_civil(2026, 1, 1);
        let utc = utc_day * 86_400 + 23 * 3_600;
        let loc = loc_day * 86_400 + 13 * 3_600;
        assert_eq!(normalize_offset(loc - utc), 50_400, "UTC+14 跨年");
        // 反向：UTC-12 跨年（1/1 05:00 UTC → 12/31 17:00）
        let utc2 = days_from_civil(2026, 1, 1) * 86_400 + 5 * 3_600;
        let loc2 = days_from_civil(2025, 12, 31) * 86_400 + 17 * 3_600;
        // -43200 与 43200 相差正好一整天，描述同一偏移；归一到区间内即为正确。
        let off = normalize_offset(loc2 - utc2);
        assert!(
            off == -43_200 || off == 43_200,
            "UTC-12 跨年应得 ±12:00，实际 {off}"
        );
        // 真正要防的是「差一整年」被算成无关的值：结果必须仍是 12 小时整。
        assert_eq!(off.abs(), 43_200, "UTC-12 跨年偏移量应为 12 小时");
    }

    /// Windows 时区探测用 `days_from_civil` 折算年积日，其正确性必须独立可验。
    #[test]
    fn days_from_civil_matches_known_dates() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1970, 1, 2), 1);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        assert_eq!(days_from_civil(2000, 1, 1), 10_957);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        // 闰年：2/29 与 3/1 相差 1 天
        assert_eq!(
            days_from_civil(2024, 3, 1) - days_from_civil(2024, 2, 29),
            1
        );
        // 非闰年 2/28 直接到 3/1
        assert_eq!(
            days_from_civil(2023, 3, 1) - days_from_civil(2023, 2, 28),
            1
        );
        // 与 format_timestamp 的日期部分互证
        assert_eq!(format_timestamp(0), "1970_01_01_00_00_00");
        // 与既有 `civil_from_days` 必须互为逆函数（覆盖闰世纪边界）
        for days in [-100_000i64, -1, 0, 1, 11_016, 36_524, 146_096, 146_097] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(
                days_from_civil(y, m as i64, d as i64),
                days,
                "往返不一致 days={days} → {y}-{m}-{d}"
            );
        }
    }

    /// 回归：Windows 分支曾只比「时分秒」，跨午夜时把 UTC+14 算成 -36000。    /// 正确做法是叠加日差（local 比 utc 晚一天 → +86400）。
    #[test]
    fn windows_style_offset_uses_day_difference() {
        let soyd = |h: i64| h * 3600;
        // UTC+14 @ UTC 12:00 → 本地次日 02:00
        assert_eq!(
            normalize_offset(soyd(2) + 86_400 - soyd(12)),
            50_400,
            "UTC+14 跨午夜"
        );
        // UTC-10 @ 同时刻 → -36000，与上式**原始钟点差相同**但不能混淆
        assert_eq!(normalize_offset(soyd(2) - soyd(12)), -36_000, "UTC-10");
    }

    #[test]
    fn detected_local_offset_is_sane() {
        // 现实时区偏移在 ±14 小时内；探测失败时退回 0，也在范围内
        let off = local_offset_secs();
        assert!(
            ((-14 * 3600)..=(14 * 3600)).contains(&off),
            "本地偏移异常: {off}"
        );
    }

    #[test]
    fn event_stamp_shape_is_hh_mm_ss() {
        let s = event_stamp();
        assert_eq!(s.len(), 8, "实际: {s}");
        assert_eq!(s.as_bytes()[2], b':');
        assert_eq!(s.as_bytes()[5], b':');
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

    /// uid 直接来自工具 stdout，可能含路径分隔符 / `..`。
    /// 默认文件名会被 `dir.join()` 拼成落盘路径，故非 hex 一律退化为 `card`。
    #[test]
    fn dump_name_rejects_non_hex_uid_to_block_path_traversal() {
        // 恶意读卡器可打印伪造的 UID 行，逃出 dumpfiles/
        assert_eq!(Paths::dump_file_name("../../escaped", "x"), "card_x.mfd");
        assert_eq!(Paths::dump_file_name("..", "x"), "card_x.mfd");
        assert_eq!(Paths::dump_file_name("/etc/passwd", "x"), "card_x.mfd");
        assert_eq!(Paths::dump_file_name("C:evil", "x"), "card_x.mfd");
        assert_eq!(Paths::dump_file_name("a/b", "x"), "card_x.mfd");
        // 合法 UID 不受影响（大小写 hex 均原样保留）
        assert_eq!(Paths::dump_file_name("a1b2c3d4", "x"), "a1b2c3d4_x.mfd");
        assert_eq!(
            Paths::dump_file_name("A1B2C3D4E5F607", "x"),
            "A1B2C3D4E5F607_x.mfd"
        );
    }

    #[test]
    fn edited_copy_name_appends_marker() {
        assert_eq!(
            Paths::edited_copy_name("card_2024_01_01.mfd", "x"),
            "card_2024_01_01_edit_x.mfd"
        );
        assert_eq!(Paths::edited_copy_name("dump.dump", "x"), "dump_edit_x.mfd");
        assert_eq!(Paths::edited_copy_name(".mfd", "x"), "dump_edit_x.mfd");
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

    // -------------------------------------------------- restrict_owner

    #[cfg(unix)]
    #[test]
    fn restrict_owner_sets_0600() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = std::env::temp_dir().join("nfc-core-restrict-test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let f = tmp.join("keys.txt");
        std::fs::write(&f, b"a0a1a2a3a4a5\n").unwrap();
        // 写前先放宽到 0644（umask 未必是这个值，这里模拟最坏情况）
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap();

        restrict_owner(&f);

        let mode = std::fs::metadata(&f).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "实际 mode: {mode:o}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(unix)]
    #[test]
    fn restrict_owner_missing_file_is_noop() {
        // 文件不存在时不 panic（调用方写入失败分支不会调它，但库函数要稳）
        let tmp = std::env::temp_dir().join("nfc-core-restrict-missing");
        let _ = std::fs::remove_dir_all(&tmp);
        restrict_owner(&tmp.join("nope"));
    }

    #[cfg(unix)]
    #[test]
    fn restrict_owner_dir_sets_0700() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = std::env::temp_dir().join("nfc-core-restrict-dir-test");
        let _ = std::fs::remove_dir_all(&tmp);
        let d = tmp.join("dumpfiles");
        std::fs::create_dir_all(&d).unwrap();
        // 模拟最坏情况：先放宽到 0755
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();

        restrict_owner_dir(&d);

        let mode = std::fs::metadata(&d).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "实际 mode: {mode:o}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn write_private_atomic_writes_and_restricts() {
        let tmp = std::env::temp_dir().join("nfc-core-atomic-test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let f = tmp.join("out.mfd");

        write_private_atomic(&f, b"payload").unwrap();

        assert_eq!(std::fs::read(&f).unwrap(), b"payload");
        assert!(!tmp.join("out.mfd.tmp").exists(), "tmp 应被 rename 走");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&f).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "实际 mode: {mode:o}");
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn fallback_dir_suffix_is_sanitized_and_isolated() {
        assert_eq!(fallback_dir_suffix(Some("alice"), 7), "alice");
        // 域分隔符 / 冒号 / 空格一律剔除
        assert_eq!(fallback_dir_suffix(Some("a/b:c d"), 7), "abcd");
        assert_eq!(fallback_dir_suffix(Some(""), 7), "pid7");
        assert_eq!(fallback_dir_suffix(None, 42), "pid42");
    }
}
