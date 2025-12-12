use macroquad::prelude::{KeyCode, vec2};

use bitflags::bitflags;

pub const BUFFER_W: u32 = 240;
pub const BUFFER_H: u32 = 140;

pub const TPS: u32 = 60;
pub const DT: f32 = 1.0 / TPS as f32;

pub const PLAYER_COUNT: usize = 2;

bitflags! {
	#[derive(Debug, Clone, Copy, PartialEq, Eq)]
	pub struct InputBits: u8 {
		const LEFT  = 1 << 0;
		const RIGHT = 1 << 1;
		const JUMP  = 1 << 2;
	}
}

impl InputBits {
	pub fn from_keyboard() -> Self {
		let mut b = InputBits::empty();
		if macroquad::prelude::is_key_down(KeyCode::A) {
			b |= InputBits::LEFT;
		}
		if macroquad::prelude::is_key_down(KeyCode::D) {
			b |= InputBits::RIGHT;
		}
		if macroquad::prelude::is_key_down(KeyCode::Space) {
			b |= InputBits::JUMP;
		}
		b
	}

	pub fn as_u8(self) -> u8 {
		self.bits()
	}

	pub fn from_u8(b: u8) -> Self {
		InputBits::from_bits_truncate(b)
	}
}

#[derive(Debug, Clone, Copy)]
pub struct Player {
	pub x: f32,
	pub y: f32,
	pub vx: f32,
	pub vy: f32,
}

impl Player {
	pub const W: f32 = 32.0;
	pub const H: f32 = 32.0;
}

#[derive(Debug, Clone, Copy)]
pub struct SimState {
	pub players: [Player; PLAYER_COUNT],
}

impl SimState {
	pub fn new() -> Self {
		let p0 = Player {
			x: 20.0,
			y: 20.0,
			vx: 0.0,
			vy: 0.0,
		};
		let p1 = Player {
			x: 100.0,
			y: 20.0,
			vx: 0.0,
			vy: 0.0,
		};
		Self { players: [p0, p1] }
	}
}

pub fn step(state: &mut SimState, inputs: [InputBits; PLAYER_COUNT]) {
	const GRAVITY: f32 = 600.0;
	const MOVE_SPEED: f32 = 90.0;
	const JUMP_SPEED: f32 = 220.0;

	for (i, p) in state.players.iter_mut().enumerate() {
		let input = inputs[i];
		let mut dx = 0i32;
		if input.contains(InputBits::LEFT) {
			dx -= 1;
		}
		if input.contains(InputBits::RIGHT) {
			dx += 1;
		}
		p.vx = dx as f32 * MOVE_SPEED;

		let on_ground = p.y + Player::H >= BUFFER_H as f32;
		if input.contains(InputBits::JUMP) && on_ground {
			p.vy = -JUMP_SPEED;
		}

		p.vy += GRAVITY * DT;

		p.x += p.vx * DT;
		p.y += p.vy * DT;

		if p.x < 0.0 {
			p.x = 0.0;
		}
		let max_x = BUFFER_W as f32 - Player::W;
		if p.x > max_x {
			p.x = max_x;
		}

		if p.y < 0.0 {
			p.y = 0.0;
			if p.vy < 0.0 {
				p.vy = 0.0;
			}
		}
		let max_y = BUFFER_H as f32 - Player::H;
		if p.y > max_y {
			p.y = max_y;
			if p.vy > 0.0 {
				p.vy = 0.0;
			}
		}
	}
}

pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
	a + (b - a) * t
}

pub fn camera_for_buffer() -> macroquad::prelude::Camera2D {
	macroquad::prelude::Camera2D {
		render_target: None, // caller fills
		zoom: vec2(2.0 / BUFFER_W as f32, 2.0 / BUFFER_H as f32),
		target: vec2(BUFFER_W as f32 / 2.0, BUFFER_H as f32 / 2.0),
		..Default::default()
	}
}
