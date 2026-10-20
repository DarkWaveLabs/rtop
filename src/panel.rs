//! Panel identities and the set of visible panels. Layout, config and app share these types.

/// One panel on the screen. The order here is the key order (1 to 8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PanelId {
    Cpu,
    History,
    Memory,
    Network,
    Storage,
    Zfs,
    Sensors,
    Processes,
}

impl PanelId {
    pub const ALL: [PanelId; 8] = [
        PanelId::Cpu,
        PanelId::History,
        PanelId::Memory,
        PanelId::Network,
        PanelId::Storage,
        PanelId::Zfs,
        PanelId::Sensors,
        PanelId::Processes,
    ];

    /// The name in the config file and in messages.
    pub fn name(self) -> &'static str {
        match self {
            PanelId::Cpu => "cpu",
            PanelId::History => "history",
            PanelId::Memory => "memory",
            PanelId::Network => "network",
            PanelId::Storage => "storage",
            PanelId::Zfs => "zfs",
            PanelId::Sensors => "sensors",
            PanelId::Processes => "processes",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }

    /// The digit key that shows or hides the panel: '1' to '8'.
    pub fn key(self) -> char {
        char::from(b'1' + self as u8)
    }

    pub fn from_key(c: char) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.key() == c)
    }

    fn bit(self) -> u16 {
        1 << self as u16
    }
}

/// A set of panels, stored as a bit mask. `Default` is every panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PanelSet(u16);

impl PanelSet {
    pub const NONE: PanelSet = PanelSet(0);
    pub const ALL: PanelSet = PanelSet((1 << PanelId::ALL.len()) - 1);

    pub fn contains(self, id: PanelId) -> bool {
        self.0 & id.bit() != 0
    }

    pub fn insert(&mut self, id: PanelId) {
        self.0 |= id.bit();
    }

    pub fn remove(&mut self, id: PanelId) {
        self.0 &= !id.bit();
    }

    pub fn toggle(&mut self, id: PanelId) {
        self.0 ^= id.bit();
    }

    pub fn iter(self) -> impl Iterator<Item = PanelId> {
        PanelId::ALL.into_iter().filter(move |&p| self.contains(p))
    }

    /// The bit mask, one bit for each `PanelId`, to share the set through an atomic.
    pub fn bits(self) -> u16 {
        self.0
    }

    /// The set from `bits`. Bits that are not a panel are dropped.
    pub fn from_bits(bits: u16) -> Self {
        PanelSet(bits & Self::ALL.0)
    }
}

impl Default for PanelSet {
    fn default() -> Self {
        Self::ALL
    }
}

impl FromIterator<PanelId> for PanelSet {
    fn from_iter<I: IntoIterator<Item = PanelId>>(iter: I) -> Self {
        let mut set = PanelSet::NONE;
        for id in iter {
            set.insert(id);
        }
        set
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_names_round_trip() {
        for id in PanelId::ALL {
            assert_eq!(PanelId::from_key(id.key()), Some(id));
            assert_eq!(PanelId::from_name(id.name()), Some(id));
        }
        assert_eq!(PanelId::Cpu.key(), '1');
        assert_eq!(PanelId::Processes.key(), '8');
        assert_eq!(PanelId::from_key('9'), None);
    }

    #[test]
    fn set_operations() {
        let mut set = PanelSet::ALL;
        set.toggle(PanelId::Zfs);
        assert!(!set.contains(PanelId::Zfs));
        set.insert(PanelId::Zfs);
        assert_eq!(set, PanelSet::ALL);
        let two: PanelSet = [PanelId::Cpu, PanelId::Memory].into_iter().collect();
        assert_eq!(
            two.iter().collect::<Vec<_>>(),
            [PanelId::Cpu, PanelId::Memory]
        );
    }

    #[test]
    fn bits_round_trip_and_drop_unknown_bits() {
        let two: PanelSet = [PanelId::Storage, PanelId::Sensors].into_iter().collect();
        assert_eq!(PanelSet::from_bits(two.bits()), two);
        assert_eq!(PanelSet::from_bits(PanelSet::ALL.bits()), PanelSet::ALL);
        assert_eq!(
            PanelSet::from_bits(u16::MAX),
            PanelSet::ALL,
            "no ninth panel"
        );
        assert_eq!(PanelSet::from_bits(0), PanelSet::NONE);
    }
}
