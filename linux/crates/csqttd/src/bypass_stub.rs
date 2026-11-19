//! Программы мимо туннеля и «только выбранные» — пока только на Linux
//! (cgroup v2 + nftables). На других системах — заглушка с тем же API.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Direct,
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
pub struct BypassParams;

impl BypassParams {
    pub fn from_config(_config: &crate::config::DaemonConfig) -> Self {
        Self
    }

    pub fn unavailable_reason(&self) -> Option<String> {
        Some("программы мимо туннеля пока есть только в версии для Linux".into())
    }

    pub fn add_process(&self, _pid: u32, _group: Group) -> anyhow::Result<()> {
        anyhow::bail!("программы мимо туннеля пока есть только в версии для Linux")
    }

    pub fn process_count(&self, _group: Group) -> u32 {
        0
    }
}

pub struct Keeper {
    group: Group,
}

impl Keeper {
    pub fn new(group: Group) -> Self {
        Self { group }
    }

    pub fn group(&self) -> Group {
        self.group
    }

    pub fn register(&self, _pid: u32) {}

    pub fn tick(&self, _params: &BypassParams) -> Vec<u32> {
        Vec::new()
    }
}

/// Проверка имени cgroup из конфига (на других системах не используется).
pub fn valid_cgroup_name(name: &str) -> bool {
    !name.is_empty() && !name.contains("..") && !name.starts_with('/')
}
