//! Процессы хоста из `/proc`: новые исполняемые файлы, подозрительные процессы и top-N.
//!
//! Опрос не видит процессы короче интервала; точный аудит exec даёт лог auditd как
//! файловый источник. Процессы контейнеров видны с хоста; путь exe берётся в mount
//! namespace процесса, поэтому ключ исполняемого файла включает контейнер.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lognara_agent::Collector;
use lognara_agent::wire::{Payload, Record, unix_nanos};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use tracing::warn;

/// Не больше стольких событий за опрос; остальные считаются в отдельном событии.
const MAX_EVENTS_PER_SCAN: usize = 50;
/// Предел множества известных исполняемых файлов и state-файла.
const MAX_KNOWN: usize = 20_000;
const MAX_CMDLINE: usize = 1024;
const SECRET_WORDS: &[&str] = &[
    "authorization",
    "bearer",
    "cookie",
    "password",
    "passwd",
    "pass",
    "pwd",
    "token",
    "secret",
    "apikey",
    "api-key",
    "api_key",
    "key",
    "auth",
    "credentials",
];

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ProcessSettings {
    pub enabled: bool,
    pub interval_seconds: u64,
    pub top: usize,
    pub top_interval_seconds: u64,
    /// Событие на каждый новый процесс; по умолчанию только новые исполняемые файлы.
    pub report_starts: bool,
    pub cmdline: bool,
    pub suspicious_dirs: Vec<String>,
}

impl Default for ProcessSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_seconds: 5,
            top: 10,
            top_interval_seconds: 60,
            report_starts: false,
            cmdline: true,
            suspicious_dirs: ["/tmp", "/var/tmp", "/dev/shm"].map(String::from).to_vec(),
        }
    }
}

impl ProcessSettings {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !(1..=300).contains(&self.interval_seconds)
            || !(10..=3600).contains(&self.top_interval_seconds)
            || self.top > 50
        {
            return Err("processes: interval 1..300 s, top_interval 10..3600 s, top <= 50");
        }
        if self.suspicious_dirs.len() > 32
            || self
                .suspicious_dirs
                .iter()
                .any(|dir| !dir.starts_with('/') || dir.len() > 4096)
        {
            return Err("processes.suspicious_dirs: up to 32 absolute paths");
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    pid: u32,
    start: u64,
}

#[derive(Clone)]
struct Info {
    ppid: u32,
    uid: u32,
    comm: String,
    exe: String,
    cmdline: String,
    container: Option<String>,
    cpu_ticks: u64,
    rss_bytes: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct State {
    executables: BTreeSet<String>,
}

pub struct Scanner {
    root: PathBuf,
    settings: ProcessSettings,
    state_path: PathBuf,
    known: BTreeSet<String>,
    known_full: bool,
    /// Первый запуск без state-файла запоминает текущие файлы без событий.
    baseline: bool,
    processes: HashMap<Key, Info>,
    previous_ticks: HashMap<Key, u64>,
    previous_at: Option<Instant>,
    tick_hz: f64,
    page: u64,
    last_top: Option<Instant>,
    /// Процессы с событиями последнего опроса и добавленные ими исполняемые файлы.
    pending: Vec<(Key, Option<String>)>,
    /// Известные файлы изменились и ещё не сохранены.
    dirty: bool,
}

impl Scanner {
    pub fn open(root: &Path, settings: ProcessSettings, state_path: PathBuf) -> Self {
        let (known, baseline) = match fs::read(&state_path) {
            Ok(bytes) => match serde_json::from_slice::<State>(&bytes) {
                Ok(state) => (state.executables, false),
                Err(error) => {
                    warn!(%error, "process state is invalid; rebuilding baseline");
                    (BTreeSet::new(), true)
                }
            },
            Err(_) => (BTreeSet::new(), true),
        };
        // SAFETY: sysconf без побочных эффектов.
        let (tick_hz, page) = unsafe {
            (
                libc::sysconf(libc::_SC_CLK_TCK),
                libc::sysconf(libc::_SC_PAGESIZE),
            )
        };
        Self {
            root: root.to_path_buf(),
            settings,
            state_path,
            known,
            known_full: false,
            baseline,
            processes: HashMap::new(),
            previous_ticks: HashMap::new(),
            previous_at: None,
            tick_hz: if tick_hz > 0 { tick_hz as f64 } else { 100.0 },
            page: if page > 0 { page as u64 } else { 4096 },
            last_top: None,
            pending: Vec::new(),
            dirty: false,
        }
    }

    /// Блокирующий опрос; возвращает записи для очереди.
    pub fn scan(&mut self, now: Instant) -> Vec<Record> {
        let first_scan = self.previous_at.is_none();
        let current = self.list();
        let mut events = Vec::new();
        let mut suppressed = 0usize;
        let mut push = |events: &mut Vec<Value>, event: Value| {
            if events.len() < MAX_EVENTS_PER_SCAN {
                events.push(event);
            } else {
                suppressed += 1;
            }
        };
        let mut known_changed = false;
        let mut next = HashMap::with_capacity(current.len());
        for (key, ticks) in current {
            let info = match self.processes.remove(&key) {
                Some(mut info) => {
                    info.cpu_ticks = ticks.0;
                    info.rss_bytes = ticks.1;
                    info
                }
                None => {
                    let Some(info) = self.details(key, ticks) else {
                        continue;
                    };
                    let executable = format!(
                        "{}:{}",
                        info.container.as_deref().unwrap_or("host"),
                        info.exe
                    );
                    let new_executable = !info.exe.is_empty()
                        && !self.known.contains(&executable)
                        && !self.known_full;
                    if new_executable {
                        if self.known.len() >= MAX_KNOWN {
                            self.known_full = true;
                            warn!(
                                "known executable limit reached; new executables are not reported"
                            );
                        } else {
                            self.known.insert(executable.clone());
                            known_changed = true;
                        }
                    }
                    let added = (new_executable && !self.known_full).then_some(executable);
                    let events_before = events.len();
                    let suspicious = self.suspicious(&info);
                    if let Some(reason) = &suspicious {
                        push(
                            &mut events,
                            event(key, &info, "process.suspicious", "warn", reason),
                        );
                    } else if new_executable && !self.baseline {
                        push(
                            &mut events,
                            event(
                                key,
                                &info,
                                "process.new_executable",
                                "info",
                                "new executable",
                            ),
                        );
                    } else if self.settings.report_starts && !first_scan {
                        push(
                            &mut events,
                            event(key, &info, "process.start", "info", "process started"),
                        );
                    }
                    if events.len() > events_before {
                        self.pending.push((key, added));
                    }
                    info
                }
            };
            next.insert(key, info);
        }
        self.processes = next;
        self.dirty |= known_changed || self.baseline;
        self.baseline = false;
        if suppressed > 0 {
            events.push(json!({
                "level": "warn",
                "action": "process.suppressed",
                "message": format!("{suppressed} process events suppressed in one scan"),
                "suppressed": suppressed,
            }));
        }
        let elapsed = self.previous_at.map(|at| now.duration_since(at));
        self.previous_at = Some(now);
        let due = self.last_top.is_none_or(|at| {
            now.duration_since(at) >= Duration::from_secs(self.settings.top_interval_seconds)
        });
        if let Some(elapsed) = elapsed.filter(|_| due) {
            self.last_top = Some(now);
            events.push(self.top(elapsed));
        }
        self.previous_ticks = self
            .processes
            .iter()
            .map(|(key, info)| (*key, info.cpu_ticks))
            .collect();
        let received_at = unix_nanos();
        events
            .into_iter()
            .map(|event| Record {
                received_at,
                origin: None,
                payload: Payload::Json(event.to_string()),
            })
            .collect()
    }

    /// pid → (cpu ticks, rss bytes) по `/proc/<pid>/stat`.
    fn list(&self) -> HashMap<Key, (u64, u64)> {
        let mut result = HashMap::new();
        let Ok(entries) = fs::read_dir(self.root.join("proc")) else {
            return result;
        };
        for entry in entries.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };
            if let Some((key, ticks, rss, _, _)) = self.stat(pid) {
                result.insert(key, (ticks, rss));
            }
        }
        result
    }

    /// (key, cpu ticks, rss bytes, ppid, comm).
    fn stat(&self, pid: u32) -> Option<(Key, u64, u64, u32, String)> {
        let text = fs::read_to_string(self.root.join(format!("proc/{pid}/stat"))).ok()?;
        // comm в скобках может содержать пробелы и скобки: поля идут после последней `)`.
        let open = text.find('(')?;
        let close = text.rfind(')')?;
        let comm = text.get(open + 1..close)?.to_owned();
        let fields: Vec<&str> = text.get(close + 2..)?.split_whitespace().collect();
        // Поле 3 (state) имеет индекс 0.
        let number = |index: usize| fields.get(index)?.parse::<u64>().ok();
        let ppid = number(1)? as u32;
        let ticks = number(11)? + number(12)?;
        let start = number(19)?;
        let rss = number(21)? * self.page;
        Some((Key { pid, start }, ticks, rss, ppid, comm))
    }

    fn details(&self, key: Key, (ticks, rss): (u64, u64)) -> Option<Info> {
        let (_, _, _, ppid, comm) = self.stat(key.pid)?;
        let base = self.root.join(format!("proc/{}", key.pid));
        let uid = fs::metadata(&base).ok()?.uid();
        let exe = fs::read_link(base.join("exe"))
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let cmdline = if self.settings.cmdline {
            fs::read(base.join("cmdline"))
                .map(|bytes| mask_cmdline(&bytes))
                .unwrap_or_default()
        } else {
            String::new()
        };
        let container = fs::read_to_string(base.join("cgroup"))
            .ok()
            .and_then(|text| container_id(&text));
        Some(Info {
            ppid,
            uid,
            comm,
            exe,
            cmdline,
            container,
            cpu_ticks: ticks,
            rss_bytes: rss,
        })
    }

    fn suspicious(&self, info: &Info) -> Option<String> {
        if info.exe.ends_with(" (deleted)") {
            return Some("executable was deleted after start".into());
        }
        self.settings
            .suspicious_dirs
            .iter()
            .find(|dir| {
                info.exe
                    .starts_with(&format!("{}/", dir.trim_end_matches('/')))
            })
            .map(|dir| format!("executable in {dir}"))
    }

    fn top(&self, elapsed: Duration) -> Value {
        let seconds = elapsed.as_secs_f64().max(0.001);
        let mut rows: Vec<(f64, Key, &Info)> = self
            .processes
            .iter()
            .map(|(key, info)| {
                let before = self
                    .previous_ticks
                    .get(key)
                    .copied()
                    .unwrap_or(info.cpu_ticks);
                let cpu =
                    info.cpu_ticks.saturating_sub(before) as f64 / self.tick_hz / seconds * 100.0;
                (cpu, *key, info)
            })
            .collect();
        let row = |(cpu, key, info): &(f64, Key, &Info)| {
            json!({
                "pid": key.pid,
                "ppid": info.ppid,
                "uid": info.uid,
                "comm": info.comm,
                "exe": info.exe,
                "cmdline": info.cmdline,
                "container": info.container,
                "cpu_percent": (cpu * 10.0).round() / 10.0,
                "rss_bytes": info.rss_bytes,
            })
        };
        rows.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.pid.cmp(&b.1.pid)));
        let top_cpu: Vec<Value> = rows.iter().take(self.settings.top).map(row).collect();
        let top_cpu_percent = rows.first().map_or(0.0, |row| row.0);
        rows.sort_by(|a, b| {
            b.2.rss_bytes
                .cmp(&a.2.rss_bytes)
                .then(a.1.pid.cmp(&b.1.pid))
        });
        let top_rss: Vec<Value> = rows.iter().take(self.settings.top).map(row).collect();
        let summary = rows
            .iter()
            .take(3)
            .map(|(_, key, info)| format!("{}({})", info.comm, key.pid))
            .collect::<Vec<_>>()
            .join(", ");
        json!({
            "level": "debug",
            "action": "process.top",
            "message": format!("{} processes; top cpu {:.1}%; top memory {summary}", rows.len(), top_cpu_percent),
            "top_cpu": top_cpu,
            "top_rss": top_rss,
            "Metrics": {
                "SchemaVersion": 2,
                "ServiceName": crate::config::SERVICE,
                "Gauges": {
                    "proc.count": rows.len(),
                    "proc.top_cpu_percent": top_cpu_percent,
                },
            },
        })
    }

    /// События опроса сохранены в spool: известные файлы можно записать на диск.
    pub fn commit(&mut self) {
        self.pending.clear();
        if std::mem::take(&mut self.dirty) {
            self.save_state();
        }
    }

    /// Запись в spool не удалась: процессы с событиями будут обнаружены и сообщены заново,
    /// а их новые исполняемые файлы снова считаются неизвестными.
    pub fn rollback(&mut self) {
        for (key, executable) in self.pending.drain(..) {
            self.processes.remove(&key);
            if let Some(executable) = executable {
                self.known.remove(&executable);
            }
        }
    }

    fn save_state(&self) {
        let result = (|| -> std::io::Result<()> {
            let dir = self.state_path.parent().unwrap_or(Path::new("."));
            fs::create_dir_all(dir)?;
            let tmp = self.state_path.with_extension("tmp");
            let mut file = fs::File::create(&tmp)?;
            serde_json::to_writer(
                &mut file,
                &State {
                    executables: self.known.clone(),
                },
            )?;
            file.flush()?;
            file.sync_all()?;
            fs::rename(&tmp, &self.state_path)?;
            fs::File::open(dir)?.sync_all()
        })();
        if let Err(error) = result {
            warn!(%error, "process state was not saved");
        }
    }
}

fn event(key: Key, info: &Info, action: &str, level: &str, reason: &str) -> Value {
    let place = info
        .container
        .as_deref()
        .map_or("host".to_owned(), |id| format!("container {id}"));
    json!({
        "level": level,
        "action": action,
        "message": format!("{reason}: {} (pid {}, uid {}, {place})", if info.exe.is_empty() { &info.comm } else { &info.exe }, key.pid, info.uid),
        "pid": key.pid,
        "ppid": info.ppid,
        "uid": info.uid,
        "comm": info.comm,
        "exe": info.exe,
        "cmdline": info.cmdline,
        "container": info.container,
    })
}

/// Аргументы через пробел; значения похожие на секреты заменены на `***`.
pub fn mask_cmdline(bytes: &[u8]) -> String {
    let args: Vec<String> = bytes
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .map(|arg| String::from_utf8_lossy(arg).into_owned())
        .collect();
    let secret = |name: &str| {
        let name = name.trim_start_matches('-').to_ascii_lowercase();
        SECRET_WORDS.iter().any(|word| {
            name == *word
                || name.ends_with(&format!("-{word}"))
                || name.ends_with(&format!("_{word}"))
        })
    };
    let mut masked = Vec::with_capacity(args.len());
    let mut hide_next = false;
    let mut user_next = false;
    for arg in args {
        if std::mem::take(&mut hide_next) {
            masked.push("***".to_owned());
            continue;
        }
        // `-u user:pass` и `--user user:pass` (curl, wget): пароль после двоеточия.
        if std::mem::take(&mut user_next) {
            masked.push(match arg.split_once(':') {
                Some((user, _)) => format!("{user}:***"),
                None => arg,
            });
            continue;
        }
        // Пароль слитно с ключом: `-pSECRET` (mysql).
        if arg.len() > 2 && arg.starts_with("-p") && !arg.starts_with("--") {
            masked.push("-p***".to_owned());
            continue;
        }
        let arg = mask_url_password(arg);
        user_next = arg == "-u" || arg == "--user";
        if let Some((name, _)) = arg.split_once(['=', ':'])
            && secret(name)
        {
            let separator = &arg[name.len()..name.len() + 1];
            masked.push(format!("{name}{separator}***"));
            continue;
        }
        hide_next = arg.starts_with('-') && secret(&arg);
        masked.push(arg);
    }
    let mut text = masked.join(" ");
    if text.len() > MAX_CMDLINE {
        let mut end = MAX_CMDLINE;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

/// `scheme://user:pass@host` → `scheme://user:***@host`.
fn mask_url_password(arg: String) -> String {
    let Some(start) = arg.find("://").map(|index| index + 3) else {
        return arg;
    };
    let rest = &arg[start..];
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let Some(at) = rest[..end].rfind('@') else {
        return arg;
    };
    let Some(colon) = rest[..at].find(':') else {
        return arg;
    };
    format!("{}{}:***{}", &arg[..start], &rest[..colon], &rest[at..])
}

/// Первые 12 символов 64-hex ID контейнера Docker, containerd, Podman или Kubernetes.
pub fn container_id(cgroup: &str) -> Option<String> {
    cgroup.lines().find_map(|line| {
        line.split(['/', '-', '.', ':'])
            .find(|part| part.len() == 64 && part.bytes().all(|b| b.is_ascii_hexdigit()))
            .map(|id| id[..12].to_owned())
    })
}

pub async fn run(
    root: PathBuf,
    settings: ProcessSettings,
    state_path: PathBuf,
    collector: Collector,
    stop: CancellationToken,
) {
    let mut ticker = tokio::time::interval(Duration::from_secs(settings.interval_seconds));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut scanner = Some(Scanner::open(&root, settings, state_path));
    // Итог записи прошлого опроса применяется в blocking-работнике: state пишется с fsync.
    let mut saved: Option<bool> = None;
    loop {
        tokio::select! {
            _ = stop.cancelled() => return,
            _ = ticker.tick() => {}
        }
        let mut owned = scanner.take().expect("scanner is returned after each scan");
        let outcome = saved.take();
        let Ok((owned, records)) = tokio::task::spawn_blocking(move || {
            match outcome {
                Some(true) => owned.commit(),
                Some(false) => owned.rollback(),
                None => {}
            }
            let records = owned.scan(Instant::now());
            (owned, records)
        })
        .await
        else {
            warn!("process scanner failed; process collection stopped");
            return;
        };
        scanner = Some(owned);
        // Переполненная очередь не копит события в памяти: процессы с событиями будут
        // обнаружены заново на следующем опросе, после восстановления доставки.
        saved = Some(match collector.save(records).await {
            Ok(()) => true,
            Err(error) => {
                warn!(%error, "process events not saved; they will be reported again");
                false
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(root: &Path, pid: u32, start: u64, ticks: u64, exe: &str, cgroup: &str) {
        let dir = root.join(format!("proc/{pid}"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("stat"),
            format!(
                "{pid} (my (proc)) S 1 1 1 0 -1 0 0 0 0 0 {ticks} 0 0 0 20 0 1 0 {start} 1000 25 0"
            ),
        )
        .unwrap();
        fs::write(dir.join("cmdline"), b"miner\0--pass\0hunter2\0token=abc\0").unwrap();
        fs::write(dir.join("cgroup"), cgroup).unwrap();
        let _ = fs::remove_file(dir.join("exe"));
        std::os::unix::fs::symlink(exe, dir.join("exe")).unwrap();
    }

    fn actions(records: &[Record]) -> Vec<(String, Value)> {
        records
            .iter()
            .map(|record| {
                let Payload::Json(json) = &record.payload else {
                    panic!()
                };
                let value: Value = serde_json::from_str(json).unwrap();
                (value["action"].as_str().unwrap().to_owned(), value)
            })
            .collect()
    }

    #[test]
    fn reports_new_and_suspicious_executables_after_silent_baseline() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let state = root.join("state/executables.json");
        let settings = ProcessSettings::default();
        process(
            root,
            10,
            100,
            0,
            "/usr/sbin/sshd",
            "0::/system.slice/ssh.service\n",
        );
        process(root, 11, 101, 0, "/tmp/.x/xmrig (deleted)", "0::/\n");
        let start = Instant::now();
        let mut scanner = Scanner::open(root, settings.clone(), state.clone());
        let first = actions(&scanner.scan(start));
        // Базовая линия молчит, но подозрительные процессы видны сразу.
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].0, "process.suspicious");
        assert_eq!(first[0].1["level"], "warn");
        // Известные файлы сохраняются только после записи событий опроса в spool.
        assert!(!state.exists());
        scanner.commit();
        assert!(state.exists());

        let container = format!("0::/system.slice/docker-{}.scope\n", "ab".repeat(32));
        process(root, 12, 200, 0, "/usr/sbin/sshd", &container);
        process(root, 13, 201, 0, "/usr/bin/tor", "0::/\n");
        process(
            root,
            10,
            100,
            500,
            "/usr/sbin/sshd",
            "0::/system.slice/ssh.service\n",
        );
        let second = actions(&scanner.scan(start + Duration::from_secs(60)));
        let new: Vec<_> = second
            .iter()
            .filter(|(action, _)| action == "process.new_executable")
            .map(|(_, value)| value["exe"].as_str().unwrap().to_owned())
            .collect();
        // sshd в контейнере - другой файл, чем sshd хоста.
        assert_eq!(new.len(), 2);
        assert!(new.contains(&"/usr/bin/tor".to_owned()));
        let container_event = second.iter().find(|(_, value)| value["pid"] == 12).unwrap();
        assert_eq!(container_event.1["container"], "abababababab");
        assert_eq!(container_event.1["comm"], "my (proc)");
        assert_eq!(container_event.1["cmdline"], "miner --pass *** token=***");

        let top = &second
            .iter()
            .find(|(action, _)| action == "process.top")
            .unwrap()
            .1;
        assert_eq!(top["top_cpu"][0]["pid"], 10);
        assert_eq!(top["Metrics"]["Gauges"]["proc.count"], 4);

        // Очередь не приняла события: следующий опрос сообщает те же процессы снова.
        scanner.rollback();
        let retried = actions(&scanner.scan(start + Duration::from_secs(61)));
        let retried: Vec<_> = retried
            .iter()
            .filter(|(action, _)| action == "process.new_executable")
            .map(|(_, value)| value["pid"].as_u64().unwrap())
            .collect();
        assert_eq!(retried.len(), 2);
        assert!(retried.contains(&12) && retried.contains(&13));
        scanner.commit();

        // После рестарта известные файлы не повторяются; подозрительный процесс виден снова.
        process(root, 14, 300, 0, "/usr/bin/tor", "0::/\n");
        let mut restarted = Scanner::open(root, settings, state);
        let after = actions(&restarted.scan(start));
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].0, "process.suspicious");
        assert_eq!(after[0].1["pid"], 11);
    }

    #[test]
    fn masks_secrets_and_bounds_cmdline() {
        assert_eq!(
            mask_cmdline(b"mysql\0-u\0root\0--password=x\0--db-token\0t\0API_KEY:abc\0"),
            "mysql -u root --password=*** --db-token *** API_KEY:***"
        );
        assert_eq!(
            mask_cmdline(
                b"curl\0-u\0admin:hunter2\0-H\0Authorization: Bearer abc\0https://bob:pw@example.com/x?a=1\0"
            ),
            "curl -u admin:*** -H Authorization:*** https://bob:***@example.com/x?a=1"
        );
        assert_eq!(mask_cmdline(b"mysql\0-pSECRET\0db\0"), "mysql -p*** db");
        assert_eq!(mask_cmdline(&vec![b'a'; 4000]).len(), MAX_CMDLINE);
        assert_eq!(container_id("0::/\n"), None);
    }
}
