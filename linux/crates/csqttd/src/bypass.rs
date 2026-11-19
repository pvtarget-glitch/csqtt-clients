//! Программы мимо туннеля и «через туннель только выбранные программы».
//!
//! Процесс, запущенный через `csqtt direct -- …`, демон переносит в отдельную
//! cgroup v2. Пока туннель поднят, nftables метят пакеты сокетов этой cgroup,
//! правило `ip rule fwmark … lookup main` уводит их в обычную таблицу, а
//! masquerade заменяет адрес туннеля, выбранный при первом поиске маршрута,
//! на адрес физического интерфейса:
//!
//! ```text
//! table inet csqtt
//!   output (route, mangle):   socket cgroupv2 level N "<cgroup>"  → mark = T
//!   postrouting (nat):        mark T, oifname != csqtt0           → masquerade
//! prio P   fwmark T lookup main
//! ```
//!
//! Дочерние процессы наследуют cgroup, поэтому мимо туннеля идёт и всё, что
//! приложение запустит само.
//!
//! Обратный режим профиля (`selected_apps_only`): в туннель идут только
//! процессы cgroup `csqtt-tunnel` (`csqtt tunnel -- …`). Их пакеты получают
//! метку T+1, правило `fwmark T+1 lookup T` заменяет общее «всё в туннель»,
//! а masquerade на csqtt0 меняет адрес физического интерфейса на адрес
//! туннеля.

use anyhow::{anyhow, bail, Context, Result};
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub const NFT_TABLE: &str = "csqtt";

/// Группа процессов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// `csqtt direct`: мимо туннеля.
    Direct,
    /// `csqtt tunnel`: через туннель (для профилей «только выбранные»).
    Tunnel,
}

impl Group {
    pub fn describe(self) -> &'static str {
        match self {
            Group::Direct => "мимо туннеля",
            Group::Tunnel => "через туннель",
        }
    }
}

#[derive(Debug, Clone)]
pub struct BypassParams {
    /// Корень cgroup v2 (обычно /sys/fs/cgroup).
    pub cgroup_root: PathBuf,
    /// cgroup «мимо туннеля» относительно корня, например `csqtt-direct`.
    pub cgroup: String,
    /// cgroup «через туннель», например `csqtt-tunnel`.
    pub tunnel_cgroup: String,
}

impl BypassParams {
    pub fn from_config(config: &crate::config::DaemonConfig) -> Self {
        Self {
            cgroup_root: config.cgroup_root.clone(),
            cgroup: config.bypass_cgroup.clone(),
            tunnel_cgroup: config.tunnel_cgroup.clone(),
        }
    }

    fn name(&self, group: Group) -> &str {
        match group {
            Group::Direct => &self.cgroup,
            Group::Tunnel => &self.tunnel_cgroup,
        }
    }

    pub fn dir(&self, group: Group) -> PathBuf {
        self.cgroup_root.join(self.name(group))
    }

    fn level(&self, group: Group) -> usize {
        self.name(group)
            .split('/')
            .filter(|part| !part.is_empty())
            .count()
    }

    /// Почему функция недоступна, или None, если всё есть.
    pub fn unavailable_reason(&self) -> Option<String> {
        if !self.cgroup_root.join("cgroup.controllers").is_file() {
            return Some(format!(
                "в {} не смонтирована cgroup v2 (нужна система с единой иерархией cgroup)",
                self.cgroup_root.display()
            ));
        }
        if find_program("nft").is_none() {
            return Some("не найдена программа nft: sudo apt install nftables".to_owned());
        }
        None
    }

    pub fn ensure_cgroup(&self, group: Group) -> Result<()> {
        let dir = self.dir(group);
        if !dir.is_dir() {
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("создание cgroup {}", dir.display()))?;
        }
        Ok(())
    }

    /// Переносит процесс (вместе с потоками) в cgroup группы.
    pub fn add_process(&self, pid: u32, group: Group) -> Result<()> {
        if let Some(reason) = self.unavailable_reason() {
            bail!("{reason}");
        }
        self.ensure_cgroup(group)?;
        let procs = self.dir(group).join("cgroup.procs");
        std::fs::write(&procs, pid.to_string())
            .with_context(|| format!("перенос процесса {pid} в {}", procs.display()))
    }

    /// Сколько процессов сейчас в группе.
    pub fn process_count(&self, group: Group) -> u32 {
        std::fs::read_to_string(self.dir(group).join("cgroup.procs"))
            .map(|text| text.lines().filter(|line| !line.trim().is_empty()).count() as u32)
            .unwrap_or(0)
    }

    /// Метки: `direct` — мимо туннеля, `tunnel` — через туннель.
    pub fn nft_ruleset(&self, interface: &str, direct: u32, tunnel: u32) -> String {
        format!(
            "table inet {NFT_TABLE} {{\n\
             \x20 chain bypass_mark {{\n\
             \x20   type route hook output priority mangle; policy accept;\n\
             \x20   socket cgroupv2 level {direct_level} \"{direct_cgroup}\" meta mark set {direct:#x}\n\
             \x20   socket cgroupv2 level {tunnel_level} \"{tunnel_cgroup}\" meta mark set {tunnel:#x}\n\
             \x20 }}\n\
             \x20 chain bypass_nat {{\n\
             \x20   type nat hook postrouting priority srcnat; policy accept;\n\
             \x20   meta mark {direct:#x} oifname != \"{interface}\" masquerade\n\
             \x20   meta mark {tunnel:#x} oifname \"{interface}\" masquerade\n\
             \x20 }}\n\
             }}\n",
            direct_level = self.level(Group::Direct),
            direct_cgroup = self.cgroup,
            tunnel_level = self.level(Group::Tunnel),
            tunnel_cgroup = self.tunnel_cgroup,
        )
    }

    pub fn install(&self, interface: &str, direct: u32, tunnel: u32) -> Result<()> {
        if let Some(reason) = self.unavailable_reason() {
            bail!("{reason}");
        }
        self.ensure_cgroup(Group::Direct)?;
        self.ensure_cgroup(Group::Tunnel)?;
        remove_table();
        nft_stdin(&self.nft_ruleset(interface, direct, tunnel))
    }
}

/// Процессы, запущенные через `csqtt direct` / `csqtt tunnel`, и их потомки.
///
/// Flatpak, Snap и запуск через systemd переносят процесс в собственную
/// systemd scope — тогда он выпал бы из группы. Раз в секунду демон находит
/// потомков зарегистрированных процессов по /proc и возвращает их в группу.
pub struct Keeper {
    group: Group,
    roots: std::sync::Mutex<Vec<Root>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Root {
    pid: u32,
    /// Время запуска из /proc/<pid>/stat: защита от повторно выданного PID.
    start: u64,
}

impl Keeper {
    pub fn new(group: Group) -> Self {
        Self {
            group,
            roots: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn group(&self) -> Group {
        self.group
    }

    pub fn register(&self, pid: u32) {
        if let Some((_, start)) = read_stat(pid) {
            let mut roots = self.lock();
            roots.retain(|root| root.pid != pid);
            roots.push(Root { pid, start });
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Root>> {
        self.roots
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Возвращает в группу ушедших из неё потомков. Возвращает их PID.
    pub fn tick(&self, params: &BypassParams) -> Vec<u32> {
        let members: std::collections::HashSet<u32> =
            std::fs::read_to_string(params.dir(self.group).join("cgroup.procs"))
                .unwrap_or_default()
                .lines()
                .filter_map(|line| line.trim().parse().ok())
                .collect();
        let roots: Vec<u32> = {
            let mut roots = self.lock();
            roots.retain(|root| read_stat(root.pid).map(|(_, start)| start) == Some(root.start));
            roots.iter().map(|root| root.pid).collect()
        };
        if roots.is_empty() && members.is_empty() {
            return Vec::new();
        }
        let parents = process_parents();
        let seeds: std::collections::HashSet<u32> = roots
            .iter()
            .copied()
            .chain(members.iter().copied())
            .collect();
        let strays = strays(&parents, &seeds, &members);
        let mut moved = Vec::new();
        for pid in strays {
            if params.add_process(pid, self.group).is_ok() {
                moved.push(pid);
            }
        }
        moved
    }
}

/// Потомки `seeds`, которых нет среди `members` (сами seeds тоже, если они
/// зарегистрированы, но выпали из группы).
fn strays(
    parents: &[(u32, u32)],
    seeds: &std::collections::HashSet<u32>,
    members: &std::collections::HashSet<u32>,
) -> Vec<u32> {
    let mut children: std::collections::HashMap<u32, Vec<u32>> = Default::default();
    for (pid, parent) in parents {
        children.entry(*parent).or_default().push(*pid);
    }
    let mut result = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut queue: Vec<u32> = seeds.iter().copied().collect();
    while let Some(pid) = queue.pop() {
        if !seen.insert(pid) {
            continue;
        }
        if !members.contains(&pid) {
            result.push(pid);
        }
        if let Some(list) = children.get(&pid) {
            queue.extend(list.iter().copied());
        }
    }
    result.sort_unstable();
    result
}

/// (PID, PPID) всех процессов.
fn process_parents() -> Vec<(u32, u32)> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<u32>().ok())
        .filter_map(|pid| read_stat(pid).map(|(parent, _)| (pid, parent)))
        .collect()
}

/// PPID и время запуска из /proc/<pid>/stat.
fn read_stat(pid: u32) -> Option<(u32, u64)> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_stat(&text)
}

fn parse_stat(text: &str) -> Option<(u32, u64)> {
    // Имя процесса в скобках может содержать пробелы и скобки.
    let rest = &text[text.rfind(')')? + 1..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // После «)»: state(0) ppid(1) … starttime(19).
    Some((fields.get(1)?.parse().ok()?, fields.get(19)?.parse().ok()?))
}

/// Убирает таблицу nftables, если она есть.
pub fn remove_table() {
    if find_program("nft").is_some() {
        let _ = nft_args(&["delete", "table", "inet", NFT_TABLE]);
    }
}

/// Имя cgroup из конфига: относительный путь из латиницы, цифр, «-», «_».
pub fn valid_cgroup_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('/')
        && name.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        })
}

fn find_program(name: &str) -> Option<PathBuf> {
    ["/usr/sbin", "/sbin", "/usr/bin", "/bin"]
        .iter()
        .map(|dir| Path::new(dir).join(name))
        .find(|path| path.is_file())
}

fn nft_args(arguments: &[&str]) -> Result<()> {
    let path = find_program("nft").ok_or_else(|| anyhow!("не найдена программа nft"))?;
    let output = Command::new(path)
        .args(arguments)
        .env_clear()
        .output()
        .context("запуск nft")?;
    if output.status.success() {
        return Ok(());
    }
    bail!(
        "nft {}: {}",
        arguments.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

fn nft_stdin(ruleset: &str) -> Result<()> {
    let path = find_program("nft").ok_or_else(|| anyhow!("не найдена программа nft"))?;
    let mut child = Command::new(path)
        .args(["-f", "-"])
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("запуск nft")?;
    child
        .stdin
        .take()
        .context("stdin nft")?
        .write_all(ruleset.as_bytes())
        .context("передача правил nft")?;
    let output = child.wait_with_output().context("ожидание nft")?;
    if output.status.success() {
        return Ok(());
    }
    bail!("nft: {}", String::from_utf8_lossy(&output.stderr).trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(cgroup: &str) -> BypassParams {
        BypassParams {
            cgroup_root: "/sys/fs/cgroup".into(),
            cgroup: cgroup.into(),
            tunnel_cgroup: "csqtt-tunnel".into(),
        }
    }

    #[test]
    fn ruleset_matches_cgroup_level_and_mark() {
        let rules = params("system.slice/csqtt-direct").nft_ruleset("csqtt0", 51880, 51881);
        assert!(rules.contains(
            "socket cgroupv2 level 2 \"system.slice/csqtt-direct\" meta mark set 0xcaa8"
        ));
        assert!(rules.contains("socket cgroupv2 level 1 \"csqtt-tunnel\" meta mark set 0xcaa9"));
        assert!(rules.contains("meta mark 0xcaa8 oifname != \"csqtt0\" masquerade"));
        assert!(rules.contains("meta mark 0xcaa9 oifname \"csqtt0\" masquerade"));
        assert!(rules.starts_with("table inet csqtt {"));
    }

    #[test]
    fn parses_proc_stat_with_odd_names() {
        let line = "4242 (my (odd) app) S 17 4242 4242 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 3 0 987654 1000 10";
        assert_eq!(parse_stat(line), Some((17, 987654)));
    }

    #[test]
    fn finds_descendants_that_left_the_group() {
        use std::collections::HashSet;
        // 10 → 11 → 12, 11 → 13; 20 посторонний.
        let parents = [(10, 1), (11, 10), (12, 11), (13, 11), (20, 1)];
        let seeds: HashSet<u32> = [10, 11, 13].into();
        let members: HashSet<u32> = [11, 13].into();
        // 10 — зарегистрирован, но ушёл в свою scope; 12 — потомок.
        assert_eq!(strays(&parents, &seeds, &members), vec![10, 12]);
    }

    #[test]
    fn cgroup_names_are_checked() {
        assert!(valid_cgroup_name("csqtt-direct"));
        assert!(valid_cgroup_name("system.slice/csqtt-direct"));
        for bad in ["", "/abs", "a/../b", "a//b", "a b", "x\"y", ".."] {
            assert!(!valid_cgroup_name(bad), "{bad}");
        }
    }
}
