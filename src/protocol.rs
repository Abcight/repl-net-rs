use serde::{Deserialize, Serialize};

pub const PLAYER_COUNT: usize = 2;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct AssignStart {
	pub player_id: u8,
	pub start_after_ms: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TickInputs {
	pub tick: u32,
	pub inputs: [u8; PLAYER_COUNT],
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct InputMsg {
	pub tick: u32,
	pub bits: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum C2S {
	Input(InputMsg),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum S2C {
	AssignStart(AssignStart),
	TickInputs(TickInputs),
}


