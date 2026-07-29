#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TeamId(pub u32);

#[derive(Debug, Clone)]
pub struct FantasyTeam {
    pub id: TeamId,
    pub name: String,
    pub budget: u8,
}
