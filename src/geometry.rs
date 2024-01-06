#[derive(Copy, Clone, Debug, PartialEq, PartialOrd, Eq, Ord, Hash)]
pub struct Pos2 {
    pub x: u16,
    pub y: u16,
}

pub const fn pos2(x: u16, y: u16) -> Pos2 {
    Pos2 { x, y }
}

#[derive(Copy, Clone, Debug, PartialEq, PartialOrd, Eq, Ord, Hash)]
pub struct Rect {
    pub min: Pos2,
    pub max: Pos2,
}

impl Rect {
    pub const fn from_min_max(min: Pos2, max: Pos2) -> Self {
        Self { min, max }
    }

    pub const fn contains(&self, pos: Pos2) -> bool {
        self.min.x <= pos.x && pos.x <= self.max.x && self.min.y <= pos.y && pos.y <= self.max.y
    }

    pub const fn contains_rect(&self, other: Self) -> bool {
        self.contains(other.min) && self.contains(other.max)
    }
}
