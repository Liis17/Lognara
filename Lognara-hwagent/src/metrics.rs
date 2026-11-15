//! Метрики хоста из `/proc` и `/sys` во встроенном формате `ServiceMetrics` (SchemaVersion 2).
//!
//! Core извлекает объект `Metrics` из attributes без правил. Меток в формате нет, поэтому
//! имена плоские (`net.eth0.rx_bytes`); число устройств каждого вида ограничено, а
//! виртуальные интерфейсы и файловые системы контейнеров исключены.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lognara_agent::Collector;
use lognara_agent::wire::{Payload, Record, unix_nanos};
use serde::Deserialize;
use serde_json::json;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;
use tracing::warn;

/// Не больше стольких устройств каждого вида (диски, сети, ФС, GPU).
const MAX_DEVICES: usize = 16;
const FS_TYPES: &[&str] = &[
    "ext2", "ext3", "ext4", "xfs", "btrfs", "zfs", "f2fs", "vfat", "exfat", "ntfs", "ntfs3", "jfs",
    "reiserfs", "bcachefs",
];
const EXCLUDED_MOUNTS: &[&str] = &["/var/lib/docker", "/var/lib/containers", "/snap", "/run"];

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct MetricsSettings {
    pub enabled: bool,
    pub interval_seconds: u64,
    /// Префиксы имён интерфейсов; `*` в конце допускается и игнорируется.
    pub exclude_interfaces: Vec<String>,
    /// Пустая строка отключает опрос NVIDIA.
    pub nvidia_smi: String,
}

impl Default for MetricsSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_seconds: 15,
            exclude_interfaces: [
                "lo", "veth", "br-", "docker", "virbr", "cni", "flannel", "cali",
            ]
            .map(String::from)
            .to_vec(),
            nvidia_smi: "/usr/bin/nvidia-smi".into(),
        }
    }
}

impl MetricsSettings {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !(5..=3600).contains(&self.interval_seconds) {
            return Err("metrics.interval_seconds must be 5..3600");
        }
        if !self.nvidia_smi.is_empty() && !Path::new(&self.nvidia_smi).is_absolute() {
            return Err("metrics.nvidia_smi must be an absolute path");
        }
        Ok(())
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct Report {
    pub gauges: BTreeMap<String, f64>,
    pub counters: BTreeMap<String, f64>,
}

impl Report {
    fn gauge(&mut self, name: String, value: f64) {
        if value.is_finite() {
            self.gauges.insert(name, value);
        }
    }

    /// Запись `ServiceMetrics`; время точки - время приёма записи.
    pub fn record(&self) -> Record {
        let counters: BTreeMap<_, _> = self
            .counters
            .iter()
            .filter(|(_, value)| **value != 0.0)
            .collect();
        let body = json!({
            "level": "debug",
            "message": "host metrics",
            "Metrics": {
                "SchemaVersion": 2,
                "ServiceName": crate::config::SERVICE,
                "Counters": counters,
                "Gauges": self.gauges,
            },
        });
        Record {
            received_at: unix_nanos(),
            origin: None,
            payload: Payload::Json(body.to_string()),
        }
    }
}

#[derive(Clone, Copy, Default)]
struct CpuTimes {
    total: u64,
    idle: u64,
    iowait: u64,
    steal: u64,
}

pub struct Sampler {
    root: PathBuf,
    settings: MetricsSettings,
    cpu: Option<CpuTimes>,
    counters: HashMap<String, u64>,
}

impl Sampler {
    pub fn new(root: &Path, settings: MetricsSettings) -> Self {
        Self {
            root: root.to_path_buf(),
            settings,
            cpu: None,
            counters: HashMap::new(),
        }
    }

    fn read(&self, path: &str) -> Option<String> {
        std::fs::read_to_string(self.root.join(path)).ok()
    }

    /// Блокирующее чтение файлов; вызывается из blocking-работника.
    pub fn sample(&mut self) -> Report {
        let mut report = Report::default();
        self.cpu(&mut report);
        self.load(&mut report);
        self.memory(&mut report);
        self.filesystems(&mut report);
        self.disks(&mut report);
        self.network(&mut report);
        self.pressure(&mut report);
        self.amd_gpus(&mut report);
        report
    }

    /// Приращение счётчика с прошлого опроса; первый опрос и сброс дают пропуск.
    fn counter(&mut self, report: &mut Report, name: String, value: u64) {
        if let Some(previous) = self.counters.insert(name.clone(), value)
            && value >= previous
        {
            report.counters.insert(name, (value - previous) as f64);
        }
    }

    fn cpu(&mut self, report: &mut Report) {
        let Some(stat) = self.read("proc/stat") else {
            return;
        };
        let mut cores = 0;
        let mut times = None;
        for line in stat.lines() {
            let mut fields = line.split_whitespace();
            match fields.next() {
                Some("cpu") => {
                    let values: Vec<u64> = fields.filter_map(|v| v.parse().ok()).collect();
                    if values.len() >= 8 {
                        times = Some(CpuTimes {
                            // guest/guest_nice уже входят в user/nice.
                            total: values[..8].iter().sum(),
                            idle: values[3] + values[4],
                            iowait: values[4],
                            steal: values[7],
                        });
                    }
                }
                Some(name) if name.starts_with("cpu") => cores += 1,
                _ => {}
            }
        }
        if cores > 0 {
            report.gauge("cpu.cores".into(), cores as f64);
        }
        let Some(now) = times else { return };
        if let Some(before) = self.cpu.replace(now)
            && now.total > before.total
        {
            let total = (now.total - before.total) as f64;
            let share = |now: u64, before: u64| now.saturating_sub(before) as f64 * 100.0 / total;
            report.gauge(
                "cpu.usage_percent".into(),
                100.0 - share(now.idle, before.idle),
            );
            report.gauge(
                "cpu.iowait_percent".into(),
                share(now.iowait, before.iowait),
            );
            report.gauge("cpu.steal_percent".into(), share(now.steal, before.steal));
        }
    }

    fn load(&self, report: &mut Report) {
        let Some(load) = self.read("proc/loadavg") else {
            return;
        };
        for (name, value) in ["load.1", "load.5", "load.15"]
            .iter()
            .zip(load.split_whitespace())
        {
            if let Ok(value) = value.parse() {
                report.gauge((*name).into(), value);
            }
        }
    }

    fn memory(&self, report: &mut Report) {
        let Some(info) = self.read("proc/meminfo") else {
            return;
        };
        let fields: HashMap<&str, f64> = info
            .lines()
            .filter_map(|line| {
                let (key, rest) = line.split_once(':')?;
                let kib: f64 = rest.split_whitespace().next()?.parse().ok()?;
                Some((key, kib * 1024.0))
            })
            .collect();
        if let (Some(total), Some(available)) = (fields.get("MemTotal"), fields.get("MemAvailable"))
            && *total > 0.0
        {
            report.gauge("memory.total_bytes".into(), *total);
            report.gauge("memory.available_bytes".into(), *available);
            report.gauge(
                "memory.used_percent".into(),
                (total - available) * 100.0 / total,
            );
        }
        if let (Some(total), Some(free)) = (fields.get("SwapTotal"), fields.get("SwapFree")) {
            report.gauge("swap.used_bytes".into(), total - free);
            if *total > 0.0 {
                report.gauge("swap.used_percent".into(), (total - free) * 100.0 / total);
            }
        }
    }

    fn filesystems(&self, report: &mut Report) {
        let Some(mounts) = self.read("proc/mounts") else {
            return;
        };
        let mut devices = HashSet::new();
        for line in mounts.lines() {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let [device, mount, kind, ..] = fields[..] else {
                continue;
            };
            let mount = unescape(mount);
            if !device.starts_with("/dev/")
                || !FS_TYPES.contains(&kind)
                || EXCLUDED_MOUNTS
                    .iter()
                    .any(|prefix| mount == *prefix || mount.starts_with(&format!("{prefix}/")))
                || !devices.insert(device)
            {
                continue;
            }
            if devices.len() > MAX_DEVICES {
                break;
            }
            let Some((total, free)) = statvfs(&self.root.join(mount.trim_start_matches('/')))
            else {
                continue;
            };
            let name = match mount.as_str() {
                "/" => "root".to_owned(),
                path => sanitize(path.trim_start_matches('/')),
            };
            report.gauge(format!("fs.{name}.free_bytes"), free);
            if total > 0.0 {
                report.gauge(
                    format!("fs.{name}.used_percent"),
                    (total - free) * 100.0 / total,
                );
            }
        }
    }

    fn disks(&mut self, report: &mut Report) {
        let Some(stats) = self.read("proc/diskstats") else {
            return;
        };
        let mut seen = 0;
        for line in stats.lines() {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 10 || !whole_disk(fields[2]) {
                continue;
            }
            seen += 1;
            if seen > MAX_DEVICES {
                break;
            }
            let device = sanitize(fields[2]);
            let sectors = |index: usize| fields[index].parse::<u64>().unwrap_or(0) * 512;
            self.counter(report, format!("disk.{device}.read_bytes"), sectors(5));
            self.counter(report, format!("disk.{device}.write_bytes"), sectors(9));
        }
    }

    fn network(&mut self, report: &mut Report) {
        let Some(table) = self.read("proc/net/dev") else {
            return;
        };
        let mut seen = 0;
        for line in table.lines().skip(2) {
            let Some((name, rest)) = line.split_once(':') else {
                continue;
            };
            let name = name.trim();
            if self.settings.exclude_interfaces.iter().any(|prefix| {
                let prefix = prefix.trim_end_matches('*');
                !prefix.is_empty() && name.starts_with(prefix)
            }) {
                continue;
            }
            seen += 1;
            if seen > MAX_DEVICES {
                break;
            }
            let values: Vec<u64> = rest
                .split_whitespace()
                .filter_map(|v| v.parse().ok())
                .collect();
            if values.len() < 9 {
                continue;
            }
            let name = sanitize(name);
            self.counter(report, format!("net.{name}.rx_bytes"), values[0]);
            self.counter(report, format!("net.{name}.tx_bytes"), values[8]);
        }
    }

    fn pressure(&self, report: &mut Report) {
        for resource in ["cpu", "memory", "io"] {
            let Some(text) = self.read(&format!("proc/pressure/{resource}")) else {
                continue;
            };
            let value = text
                .lines()
                .find(|line| line.starts_with("some "))
                .and_then(|line| {
                    line.split_whitespace()
                        .find_map(|f| f.strip_prefix("avg10="))
                })
                .and_then(|value| value.parse().ok());
            if let Some(value) = value {
                report.gauge(format!("pressure.{resource}.some_avg10"), value);
            }
        }
    }

    fn amd_gpus(&self, report: &mut Report) {
        let Ok(cards) = std::fs::read_dir(self.root.join("sys/class/drm")) else {
            return;
        };
        let mut names: Vec<String> = cards
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .filter(|name| {
                name.strip_prefix("card")
                    .is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()) && !n.is_empty())
            })
            .collect();
        names.sort();
        for card in names.into_iter().take(MAX_DEVICES) {
            let device = format!("sys/class/drm/{card}/device");
            let value = |file: &str| -> Option<f64> {
                self.read(&format!("{device}/{file}"))?.trim().parse().ok()
            };
            let Some(busy) = value("gpu_busy_percent") else {
                continue;
            };
            let name = format!("gpu.{card}");
            report.gauge(format!("{name}.util_percent"), busy);
            if let (Some(used), Some(total)) =
                (value("mem_info_vram_used"), value("mem_info_vram_total"))
            {
                report.gauge(format!("{name}.memory_used_bytes"), used);
                if total > 0.0 {
                    report.gauge(format!("{name}.memory_used_percent"), used * 100.0 / total);
                }
            }
        }
    }
}

/// Разбор вывода `nvidia-smi --query-gpu=index,utilization.gpu,memory.used,memory.total,
/// temperature.gpu,power.draw --format=csv,noheader,nounits`.
pub fn parse_nvidia(output: &str, report: &mut Report) {
    for line in output.lines().take(MAX_DEVICES) {
        let fields: Vec<&str> = line.split(',').map(str::trim).collect();
        let [index, util, used, total, temperature, power] = fields[..] else {
            continue;
        };
        if index.is_empty() || !index.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let name = format!("gpu.nvidia{index}");
        let number = |text: &str| text.parse::<f64>().ok();
        if let Some(util) = number(util) {
            report.gauge(format!("{name}.util_percent"), util);
        }
        if let (Some(used), Some(total)) = (number(used), number(total)) {
            report.gauge(format!("{name}.memory_used_bytes"), used * 1024.0 * 1024.0);
            if total > 0.0 {
                report.gauge(format!("{name}.memory_used_percent"), used * 100.0 / total);
            }
        }
        if let Some(temperature) = number(temperature) {
            report.gauge(format!("{name}.temperature_c"), temperature);
        }
        if let Some(power) = number(power) {
            report.gauge(format!("{name}.power_w"), power);
        }
    }
}

async fn nvidia(path: &str, report: &mut Report) {
    if path.is_empty() || !Path::new(path).exists() {
        return;
    }
    let mut command = Command::new(path);
    command
        .args([
            "--query-gpu=index,utilization.gpu,memory.used,memory.total,temperature.gpu,power.draw",
            "--format=csv,noheader,nounits",
        ])
        .kill_on_drop(true);
    match tokio::time::timeout(Duration::from_secs(5), command.output()).await {
        Ok(Ok(output)) if output.status.success() => {
            parse_nvidia(&String::from_utf8_lossy(&output.stdout), report)
        }
        Ok(Ok(output)) => warn!(status = %output.status, "nvidia-smi failed"),
        Ok(Err(error)) => warn!(%error, "nvidia-smi could not start"),
        Err(_) => warn!("nvidia-smi timed out"),
    }
}

/// Опрос до отмены. При переполненной очереди отчёт пропускается: метрики не копятся.
pub async fn run(
    root: PathBuf,
    settings: MetricsSettings,
    collector: Collector,
    stop: CancellationToken,
) {
    let nvidia_path = settings.nvidia_smi.clone();
    let mut ticker = tokio::time::interval(Duration::from_secs(settings.interval_seconds));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut sampler = Some(Sampler::new(&root, settings));
    loop {
        tokio::select! {
            _ = stop.cancelled() => return,
            _ = ticker.tick() => {}
        }
        let mut owned = sampler
            .take()
            .expect("sampler is returned after each sample");
        let Ok((owned, mut report)) = tokio::task::spawn_blocking(move || {
            let report = owned.sample();
            (owned, report)
        })
        .await
        else {
            warn!("metrics sampler failed; metrics stopped");
            return;
        };
        sampler = Some(owned);
        nvidia(&nvidia_path, &mut report).await;
        if let Err(error) = collector.save(vec![report.record()]).await {
            warn!(%error, "host metrics report skipped");
        }
    }
}

fn whole_disk(name: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let letters = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase());
    if let Some(rest) = name.strip_prefix("nvme") {
        return rest
            .split_once('n')
            .is_some_and(|(controller, namespace)| digits(controller) && digits(namespace));
    }
    if let Some(rest) = name.strip_prefix("mmcblk") {
        return digits(rest);
    }
    ["sd", "vd", "xvd", "hd"]
        .iter()
        .any(|prefix| name.strip_prefix(prefix).is_some_and(letters))
}

/// Только `[A-Za-z0-9_.-]`, не длиннее 48 байт: полное имя метрики остаётся до 128 байт.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "_-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .take(48)
        .collect()
}

/// `/proc/mounts` экранирует пробел и спецсимволы как `\040`.
fn unescape(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && i + 3 < bytes.len()
            && let Some(value) = std::str::from_utf8(&bytes[i + 1..i + 4])
                .ok()
                .and_then(|digits| u8::from_str_radix(digits, 8).ok())
        {
            out.push(value);
            i += 4;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn statvfs(path: &Path) -> Option<(f64, f64)> {
    let path = CString::new(path.as_os_str().as_encoded_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: путь - корректная C-строка, stat - выделенная структура нужного типа.
    if unsafe { libc::statvfs(path.as_ptr(), &mut stat) } != 0 {
        return None;
    }
    let block = stat.f_frsize as f64;
    Some((stat.f_blocks as f64 * block, stat.f_bavail as f64 * block))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, path: &str, text: &str) {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn fixture(root: &Path, idle: u64, rx: u64, read_sectors: u64) {
        write(
            root,
            "proc/stat",
            &format!(
                "cpu  100 0 100 {idle} 50 0 0 10 0 0\ncpu0 1 0 1 1 0 0 0 0 0 0\ncpu1 1 0 1 1 0 0 0 0 0 0\nintr 1\n"
            ),
        );
        write(root, "proc/loadavg", "0.50 0.25 0.10 1/100 42\n");
        write(
            root,
            "proc/meminfo",
            "MemTotal:       1000 kB\nMemAvailable:    250 kB\nSwapTotal:       100 kB\nSwapFree:         50 kB\n",
        );
        write(
            root,
            "proc/mounts",
            "/dev/sda1 / ext4 rw 0 0\noverlay /var/lib/docker/overlay2/x/merged overlay rw 0 0\ntmpfs /run tmpfs rw 0 0\n/dev/sda1 /mnt/bind ext4 rw 0 0\n",
        );
        write(
            root,
            "proc/diskstats",
            &format!(
                "   8       0 sda 1 0 {read_sectors} 0 1 0 8 0 0 0 0\n   8       1 sda1 1 0 5 0 1 0 8 0 0 0 0\n   7       0 loop0 1 0 5 0 1 0 8 0 0 0 0\n"
            ),
        );
        write(
            root,
            "proc/net/dev",
            &format!(
                "Inter-|   Receive\n face |bytes packets\n    lo: 9 0 0 0 0 0 0 0 9 0 0 0 0 0 0 0\n  eth0: {rx} 0 0 0 0 0 0 0 20 0 0 0 0 0 0 0\nveth12ab: 5 0 0 0 0 0 0 0 5 0 0 0 0 0 0 0\n"
            ),
        );
        write(
            root,
            "proc/pressure/cpu",
            "some avg10=1.50 avg60=0.00 avg300=0.00 total=1\n",
        );
        write(root, "sys/class/drm/card0/device/gpu_busy_percent", "37\n");
        write(
            root,
            "sys/class/drm/card0/device/mem_info_vram_used",
            "512\n",
        );
        write(
            root,
            "sys/class/drm/card0/device/mem_info_vram_total",
            "1024\n",
        );
    }

    #[test]
    fn samples_host_counters_as_deltas_and_skips_virtual_devices() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path(), 800, 1000, 10);
        let mut sampler = Sampler::new(dir.path(), MetricsSettings::default());
        let first = sampler.sample();
        assert!(first.counters.is_empty());
        assert!(!first.gauges.contains_key("cpu.usage_percent"));
        assert_eq!(first.gauges["cpu.cores"], 2.0);
        assert_eq!(first.gauges["memory.used_percent"], 75.0);
        assert_eq!(first.gauges["swap.used_percent"], 50.0);
        assert_eq!(first.gauges["load.5"], 0.25);
        assert_eq!(first.gauges["pressure.cpu.some_avg10"], 1.5);
        assert_eq!(first.gauges["gpu.card0.util_percent"], 37.0);
        assert_eq!(first.gauges["gpu.card0.memory_used_percent"], 50.0);
        assert!(first.gauges.contains_key("fs.root.used_percent"));
        assert!(
            first
                .gauges
                .keys()
                .all(|name| !name.contains("docker") && !name.contains("bind"))
        );

        fixture(dir.path(), 900, 1600, 14);
        let second = sampler.sample();
        // За интервал прошло 100 тиков, и все они idle.
        assert_eq!(second.gauges["cpu.usage_percent"], 0.0);
        assert_eq!(second.counters["net.eth0.rx_bytes"], 600.0);
        assert_eq!(second.counters["net.eth0.tx_bytes"], 0.0);
        assert_eq!(second.counters["disk.sda.read_bytes"], 4.0 * 512.0);
        assert!(second.counters.keys().all(|name| !name.contains("lo.")
            && !name.contains("veth")
            && !name.contains("loop")
            && !name.contains("sda1")));

        let record = second.record();
        let Payload::Json(json) = record.payload else {
            panic!()
        };
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["Metrics"]["SchemaVersion"], 2);
        assert_eq!(value["Metrics"]["Counters"]["net.eth0.rx_bytes"], 600.0);
        // Нулевые приращения опускаются, как у сервисов с ServiceMetrics.
        assert!(
            value["Metrics"]["Counters"]
                .get("net.eth0.tx_bytes")
                .is_none()
        );
    }

    #[test]
    fn counter_reset_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path(), 800, 1000, 10);
        let mut sampler = Sampler::new(dir.path(), MetricsSettings::default());
        sampler.sample();
        fixture(dir.path(), 900, 10, 10);
        assert!(!sampler.sample().counters.contains_key("net.eth0.rx_bytes"));
    }

    #[test]
    fn parses_nvidia_smi_and_ignores_unavailable_values() {
        let mut report = Report::default();
        parse_nvidia(
            "0, 91, 2048, 8192, 70, 180.5\n1, [N/A], 0, 8192, 40, [N/A]\n",
            &mut report,
        );
        assert_eq!(report.gauges["gpu.nvidia0.util_percent"], 91.0);
        assert_eq!(report.gauges["gpu.nvidia0.memory_used_percent"], 25.0);
        assert_eq!(report.gauges["gpu.nvidia0.power_w"], 180.5);
        assert!(!report.gauges.contains_key("gpu.nvidia1.util_percent"));
        assert_eq!(report.gauges["gpu.nvidia1.temperature_c"], 40.0);
    }

    #[test]
    fn recognizes_whole_disks_only() {
        for disk in ["sda", "vdb", "xvdc", "nvme0n1", "mmcblk0"] {
            assert!(whole_disk(disk), "{disk}");
        }
        for part in ["sda1", "nvme0n1p1", "mmcblk0p1", "loop0", "dm-0", "sr0"] {
            assert!(!whole_disk(part), "{part}");
        }
        assert_eq!(unescape("/mnt/my\\040disk"), "/mnt/my disk");
        assert_eq!(sanitize("mnt/my disk"), "mnt_my_disk");
    }
}
